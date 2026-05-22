//! Packet fragmentation and reassembly.
//!
//! Large packets that exceed [`EndpointConfig::fragment_above`] are split into
//! multiple UDP datagrams called fragments. Each fragment carries a
//! [`FragmentHeader`] so the receiver can detect fragmented traffic and
//! accumulate pieces until the full logical packet is available.
//!
//! ## Wire Layout
//!
//! ### Fragment datagram (all fragments)
//!
//! ```text
//! +--------------------+
//! | FragmentHeader (5) |  -- prefix byte(0x01), seq(LE u16), id, count
//! +--------------------+
//! | PacketHeader (var) |  -- present ONLY in fragment 0 (carries ACK info)
//! +--------------------+
//! | Fragment payload   |
//! +--------------------+
//! ```
//!
//! The first byte's LSB being `1` distinguishes fragment datagrams from regular
//! packet datagrams (where bit 0 is always `0` in the prefix byte).
//!
//! ### FragmentHeader encoding (5 bytes, little-endian)
//!
//! ```text
//! Byte 0: 0x01       (fragment flag)
//! Byte 1: seq[0]     (sequence low byte, LE)
//! Byte 2: seq[1]     (sequence high byte, LE)
//! Byte 3: fragment_id      (0 .. num_fragments-1)
//! Byte 4: num_fragments    (1 .. 255)
//! ```
//!
//! ## Reassembly
//!
//! Fragments are held in a [`FragmentReassemblyBuffer`] - a power-of-two ring
//! indexed by `sequence & (capacity - 1)`. Each [`ReassemblyData`] slot owns
//! a flat byte slab (`fragment_data: Box<[u8]>`, size = `max_fragments *
//! fragment_size`) preallocated once at buffer construction. No heap activity
//! occurs during steady-state fragment receipt or reassembly.
//!
//! The [`SequenceBuffer`] is used only for sequence-window bookkeeping
//! (`SequenceBuffer<()>`); actual payload bytes live in the separate slab.
//!
//! Duplicate fragments are silently dropped via a `[u32; 8]` bitmask.
//! Out-of-order arrival is handled transparently.

use crate::packet_queue::PacketQueue;
use crate::sequence_buffer::SequenceBuffer;

/// Fragment header size
pub const FRAGMENT_HEADER_BYTES: usize = 5;

/// Fragment header
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FragmentHeader {
    /// Sequence number of the original packet
    pub sequence: u16,
    /// Fragment ID (0 to num_fragments-1)
    pub fragment_id: u8,
    /// Total number of fragments
    pub num_fragments: u8,
}

impl FragmentHeader {
    /// Write fragment header to a fixed-size buffer slice (allocation-free).
    ///
    /// Returns the number of bytes written, or `None` if the buffer is too
    /// small. Buffer must be at least [`FRAGMENT_HEADER_BYTES`] (5 bytes).
    ///
    /// This is the hot-path method used by [`Endpoint`](crate::endpoint::Endpoint).
    #[inline]
    pub fn write_to_slice(&self, buffer: &mut [u8]) -> Option<usize> {
        if buffer.len() < FRAGMENT_HEADER_BYTES {
            return None;
        }
        // Prefix byte with fragment flag set (bit 0 = 1)
        buffer[0] = 1;
        let seq_bytes = self.sequence.to_le_bytes();
        buffer[1] = seq_bytes[0];
        buffer[2] = seq_bytes[1];
        buffer[3] = self.fragment_id;
        buffer[4] = self.num_fragments;
        Some(FRAGMENT_HEADER_BYTES)
    }

    /// Read fragment header from buffer
    #[inline]
    pub fn read(data: &[u8]) -> Option<(Self, usize)> {
        if data.len() < FRAGMENT_HEADER_BYTES {
            return None;
        }

        // Check fragment flag
        if (data[0] & 1) != 1 {
            return None;
        }

        let sequence = u16::from_le_bytes([data[1], data[2]]);
        let fragment_id = data[3];
        let num_fragments = data[4];

        Some((
            Self {
                sequence,
                fragment_id,
                num_fragments,
            },
            FRAGMENT_HEADER_BYTES,
        ))
    }
}

/// Per-slot state for reassembling a fragmented packet.
///
/// Each `ReassemblyData` owns a flat slab (`fragment_data`) and a length
/// array (`fragment_lens`) preallocated once when [`FragmentReassemblyBuffer`]
/// is constructed. Resetting a slot for a new sequence clears metadata fields
/// only; the preallocated buffers are retained and reused.
pub(crate) struct ReassemblyData {
    /// Sequence number being reassembled.
    pub(crate) sequence: u16,
    /// ACK from first fragment header.
    pub(crate) ack: u16,
    /// ACK bits from first fragment header.
    pub(crate) ack_bits: u32,
    /// Total fragment count declared on the wire.
    pub(crate) num_fragments: u8,
    /// Number of fragments received so far.
    pub(crate) num_fragments_received: u8,
    /// Bitmask of received fragment IDs.
    /// Bit `id & 31` of word `id >> 5` is set when fragment `id` arrived.
    pub(crate) fragment_received: [u32; 8],
    /// Flat fragment payload slab. Fragment `i` occupies bytes
    /// `[i * fragment_size .. i * fragment_size + fragment_lens[i]]`.
    /// Preallocated once; never reallocated.
    fragment_data: Box<[u8]>,
    /// Byte length of each stored fragment. Preallocated once.
    fragment_lens: Box<[u16]>,
    /// Cached fragment size for offset arithmetic.
    fragment_size: usize,
}

impl ReassemblyData {
    /// Allocate a new slot with preallocated slab storage.
    ///
    /// Called once per slot in [`FragmentReassemblyBuffer::new`].
    fn new_preallocated(max_fragments: usize, fragment_size: usize) -> Self {
        Self {
            sequence: 0,
            ack: 0,
            ack_bits: 0,
            num_fragments: 0,
            num_fragments_received: 0,
            fragment_received: [0u32; 8],
            fragment_data: vec![0u8; max_fragments * fragment_size].into_boxed_slice(),
            fragment_lens: vec![0u16; max_fragments].into_boxed_slice(),
            fragment_size,
        }
    }

    /// Reset this slot for a new sequence number.
    ///
    /// Clears all metadata fields. The pre-allocated `fragment_data` and
    /// `fragment_lens` slabs are retained; only the active length entries
    /// (0..num_fragments) are zeroed.
    pub(crate) fn reset(&mut self, sequence: u16, ack: u16, ack_bits: u32, num_fragments: u8) {
        self.sequence = sequence;
        self.ack = ack;
        self.ack_bits = ack_bits;
        self.num_fragments = num_fragments;
        self.num_fragments_received = 0;
        self.fragment_received = [0u32; 8];
        for len in &mut self.fragment_lens[..num_fragments as usize] {
            *len = 0;
        }
    }

    /// Returns `true` if fragment `id` has already been received.
    #[inline]
    pub(crate) fn has_fragment(&self, id: u8) -> bool {
        let word = (id >> 5) as usize;
        let bit = id & 31;
        (self.fragment_received[word] & (1 << bit)) != 0
    }

    /// Mark fragment `id` as received in the bitmask.
    #[inline]
    pub(crate) fn mark_fragment(&mut self, id: u8) {
        let word = (id >> 5) as usize;
        let bit = id & 31;
        self.fragment_received[word] |= 1 << bit;
    }

    /// Returns `true` when all expected fragments have arrived.
    #[inline]
    pub(crate) fn is_complete(&self) -> bool {
        self.num_fragments_received == self.num_fragments
    }

    /// Write `src` into the slab at the offset for fragment `id`.
    ///
    /// Allocation-free. Bytes beyond `fragment_size` are silently truncated.
    #[inline]
    pub(crate) fn store_fragment(&mut self, id: u8, src: &[u8]) {
        let offset = id as usize * self.fragment_size;
        let len = src.len().min(self.fragment_size);
        self.fragment_data[offset..offset + len].copy_from_slice(&src[..len]);
        self.fragment_lens[id as usize] = len as u16;
    }

    /// Copy reassembled payload into `dest`, returning the number of bytes
    /// written. Used in `receive_fragment` when endpoint writes directly into an
    /// [`incoming_queue`](crate::packet_queue::PacketQueue) slot.
    ///
    /// Allocation-free.
    pub(crate) fn copy_to(&self, dest: &mut [u8]) -> usize {
        let mut pos = 0;
        for i in 0..self.num_fragments as usize {
            let len = self.fragment_lens[i] as usize;
            let src_off = i * self.fragment_size;
            dest[pos..pos + len].copy_from_slice(&self.fragment_data[src_off..src_off + len]);
            pos += len;
        }
        pos
    }
}

/// Fragment reassembly buffer.
///
/// Uses a [`SequenceBuffer<()>`] for sequence-window bookkeeping and a
/// separate `Box<[ReassemblyData]>` for the preallocated payload slabs.
/// Slot index is always `sequence & (capacity - 1)`.
///
/// All `ReassemblyData` slots are fully preallocated in [`new`]; no heap
/// activity occurs during steady-state fragment processing.
pub(crate) struct FragmentReassemblyBuffer {
    /// Tracks which sequences are active and manages the eviction window.
    tracker: SequenceBuffer<()>,
    /// Preallocated per-slot reassembly state. Indexed by `seq & (capacity - 1)`.
    slots: Box<[ReassemblyData]>,
    /// Cached buffer capacity (= `slots.len()`). Must be a power of two.
    capacity: usize,
    max_fragments: usize,
}

impl FragmentReassemblyBuffer {
    /// Create a new fragment reassembly buffer with all slabs preallocated.
    pub fn new(size: usize, fragment_size: usize, max_fragments: usize) -> Self {
        let slots: Vec<ReassemblyData> = (0..size)
            .map(|_| ReassemblyData::new_preallocated(max_fragments, fragment_size))
            .collect();

        Self {
            tracker: SequenceBuffer::new(size),
            slots: slots.into_boxed_slice(),
            capacity: size,
            max_fragments,
        }
    }

    /// Process a received fragment.
    ///
    /// On success, writes the reassembled payload into `incoming_queue` via
    /// [`PacketQueue::write_slot`] and returns `Some(payload_bytes)`. Returns `None` if
    /// reassembly is not yet complete, the fragment is invalid or duplicate, or the
    /// incoming queue is full (drop with `log::warn!`).
    ///
    /// Allocation-free on the hot path.
    pub fn process_fragment(
        &mut self,
        header: &FragmentHeader,
        fragment_data: &[u8],
        ack: u16,
        ack_bits: u32,
        incoming_queue: &mut PacketQueue,
    ) -> Option<usize> {
        // Validate fragment metadata.
        if header.num_fragments == 0 || header.num_fragments as usize > self.max_fragments {
            log::warn!(
                "Invalid num_fragments: {} (max: {})",
                header.num_fragments,
                self.max_fragments
            );
            return None;
        }

        if header.fragment_id >= header.num_fragments {
            log::warn!(
                "Invalid fragment_id: {} >= {}",
                header.fragment_id,
                header.num_fragments
            );
            return None;
        }

        let is_new = !self.tracker.exists(header.sequence);

        if is_new {
            if !self.tracker.can_insert(header.sequence) {
                log::warn!(
                    "Reassembly buffer full or sequence too old: {}",
                    header.sequence
                );
                return None;
            }
            // Advance the tracker window. Return value is () - discard.
            let _ = self.tracker.insert(header.sequence);

            // Reset the preallocated slab for this slot (no allocation).
            let idx = header.sequence as usize & (self.capacity - 1);
            self.slots[idx].reset(header.sequence, ack, ack_bits, header.num_fragments);
        } else {
            let idx = header.sequence as usize & (self.capacity - 1);
            let slot = &self.slots[idx];
            // Guard against hash collision if tracker and slots are ever desynced.
            if slot.sequence != header.sequence {
                log::warn!(
                    "Reassembly slot sequence mismatch: expected {}, got {}",
                    header.sequence,
                    slot.sequence
                );
                return None;
            }
            if slot.num_fragments != header.num_fragments {
                log::warn!(
                    "Fragment count mismatch: {} vs {}",
                    slot.num_fragments,
                    header.num_fragments
                );
                return None;
            }
        }

        let idx = header.sequence as usize & (self.capacity - 1);

        // Drop duplicates.
        if self.slots[idx].has_fragment(header.fragment_id) {
            log::debug!(
                "Duplicate fragment {} for sequence {}",
                header.fragment_id,
                header.sequence
            );
            return None;
        }

        // Store into preallocated slab - allocation-free.
        self.slots[idx].store_fragment(header.fragment_id, fragment_data);
        self.slots[idx].mark_fragment(header.fragment_id);
        self.slots[idx].num_fragments_received += 1;

        if self.slots[idx].is_complete() {
            // Write reassembled payload directly into the incoming queue slot - zero-alloc.
            // See ADR-002 for rationale on passing incoming_queue as parameter.
            let mut payload_len = 0usize;
            let delivered = incoming_queue.write_slot(header.sequence, |buf| {
                let n = self.slots[idx].copy_to(buf);
                payload_len = n;
                n
            });
            self.tracker.remove(header.sequence);
            if delivered {
                Some(payload_len)
            } else {
                log::warn!(
                    "incoming_queue full, dropping reassembled packet seq={}",
                    header.sequence
                );
                None
            }
        } else {
            None
        }
    }

    /// Reset the buffer, discarding all in-progress reassembly.
    ///
    /// Preallocated slabs are retained.
    pub fn reset(&mut self) {
        self.tracker.reset();
        // Slot data is overwritten by reset() on next use - no need to clear now.
    }
}

/// Fragment a packet into multiple parts.
///
/// Returns `None` if `data` is empty, `fragment_size` is zero, or the number
/// of fragments would exceed `max_fragments` or 255.
///
/// Used only in unit tests. Hot path in `endpoint.rs` inlines fragment
/// creation directly into `PacketQueue::write_slot` calls.
#[allow(dead_code)]
pub(crate) fn fragment_packet(
    sequence: u16,
    data: &[u8],
    fragment_size: usize,
    max_fragments: usize,
) -> Option<Vec<(FragmentHeader, Vec<u8>)>> {
    if data.is_empty() || fragment_size == 0 {
        return None;
    }

    let num_fragments = data.len().div_ceil(fragment_size);

    if num_fragments > max_fragments || num_fragments > 255 {
        return None;
    }

    let mut fragments = Vec::with_capacity(num_fragments);

    for i in 0..num_fragments {
        let start = i * fragment_size;
        let end = (start + fragment_size).min(data.len());

        let header = FragmentHeader {
            sequence,
            fragment_id: i as u8,
            num_fragments: num_fragments as u8,
        };

        fragments.push((header, data[start..end].to_vec()));
    }

    Some(fragments)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet_queue::PacketQueue;

    /// Helper: drain first payload from a PacketQueue into a Vec.
    fn drain_first(q: &mut PacketQueue) -> Vec<u8> {
        let mut out = Vec::new();
        q.drain(|_, data| {
            if out.is_empty() {
                out.extend_from_slice(data);
            }
        });
        out
    }

    #[test]
    fn test_fragment_header_roundtrip() {
        let header = FragmentHeader {
            sequence: 12345,
            fragment_id: 5,
            num_fragments: 16,
        };

        let mut buffer = [0u8; FRAGMENT_HEADER_BYTES];
        let written = header.write_to_slice(&mut buffer).unwrap();

        let (parsed, size) = FragmentHeader::read(&buffer[..written]).unwrap();
        assert_eq!(size, FRAGMENT_HEADER_BYTES);
        assert_eq!(header, parsed);
    }

    #[test]
    fn test_fragment_packet() {
        let data = vec![0u8; 3000];
        let fragments = fragment_packet(100, &data, 1024, 16).unwrap();

        assert_eq!(fragments.len(), 3);
        assert_eq!(fragments[0].0.fragment_id, 0);
        assert_eq!(fragments[1].0.fragment_id, 1);
        assert_eq!(fragments[2].0.fragment_id, 2);

        assert_eq!(fragments[0].1.len(), 1024);
        assert_eq!(fragments[1].1.len(), 1024);
        assert_eq!(fragments[2].1.len(), 952); // 3000 - 2048
    }

    #[test]
    fn test_reassembly() {
        let data = vec![1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let fragments = fragment_packet(42, &data, 4, 16).unwrap();

        let mut reassembly = FragmentReassemblyBuffer::new(64, 4, 16);
        let mut q = PacketQueue::new(16, 4096);

        // Process fragments out of order
        let result0 = reassembly.process_fragment(&fragments[2].0, &fragments[2].1, 0, 0, &mut q);
        assert!(result0.is_none());

        let result1 = reassembly.process_fragment(&fragments[0].0, &fragments[0].1, 0, 0, &mut q);
        assert!(result1.is_none());

        let result2 = reassembly.process_fragment(&fragments[1].0, &fragments[1].1, 0, 0, &mut q);
        assert!(result2.is_some());

        let reassembled = drain_first(&mut q);
        assert_eq!(reassembled, data);
    }

    #[test]
    fn test_reassembly_in_order() {
        let data = vec![1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let fragments = fragment_packet(42, &data, 4, 16).unwrap();

        let mut reassembly = FragmentReassemblyBuffer::new(64, 4, 16);
        let mut q = PacketQueue::new(16, 4096);

        // Process fragments in order
        let result0 = reassembly.process_fragment(&fragments[0].0, &fragments[0].1, 0, 0, &mut q);
        assert!(result0.is_none());

        let result1 = reassembly.process_fragment(&fragments[1].0, &fragments[1].1, 0, 0, &mut q);
        assert!(result1.is_none());

        let result2 = reassembly.process_fragment(&fragments[2].0, &fragments[2].1, 0, 0, &mut q);
        assert!(result2.is_some());

        let reassembled = drain_first(&mut q);
        assert_eq!(reassembled, data);
    }

    #[test]
    fn test_duplicate_fragment() {
        let data = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
        let fragments = fragment_packet(42, &data, 4, 16).unwrap();

        let mut reassembly = FragmentReassemblyBuffer::new(64, 4, 16);
        let mut q = PacketQueue::new(16, 4096);

        // Process first fragment
        reassembly.process_fragment(&fragments[0].0, &fragments[0].1, 0, 0, &mut q);

        // Duplicate should return None
        let duplicate = reassembly.process_fragment(&fragments[0].0, &fragments[0].1, 0, 0, &mut q);
        assert!(duplicate.is_none());
    }

    #[test]
    fn test_too_many_fragments() {
        let data = vec![0u8; 1000];

        // Should fail if too many fragments needed
        let result = fragment_packet(0, &data, 10, 16);
        assert!(result.is_none());
    }

    #[test]
    fn test_large_packet_reassembly() {
        // Test with a larger packet that has an odd size
        let data: Vec<u8> = (0..1000).map(|i| (i % 256) as u8).collect();
        let fragments = fragment_packet(100, &data, 128, 16).unwrap();

        assert_eq!(fragments.len(), 8); // 1000 / 128 = 7.8125, rounds up to 8

        let mut reassembly = FragmentReassemblyBuffer::new(64, 128, 16);
        let mut q = PacketQueue::new(16, 4096);

        // Process in reverse order
        for i in (0..fragments.len()).rev() {
            let result =
                reassembly.process_fragment(&fragments[i].0, &fragments[i].1, 0, 0, &mut q);
            if i == 0 {
                assert!(result.is_some());
            } else {
                assert!(result.is_none());
            }
        }

        let reassembled = drain_first(&mut q);
        assert_eq!(reassembled, data);
    }

    #[test]
    fn test_reassembly_reset_reuses_slab() {
        // Verify reset() retains preallocated slabs and allows reuse.
        let data = vec![42u8; 8];
        let fragments = fragment_packet(1, &data, 4, 16).unwrap();

        let mut buf = FragmentReassemblyBuffer::new(64, 4, 16);
        let mut q = PacketQueue::new(16, 4096);

        for frag in &fragments {
            buf.process_fragment(&frag.0, &frag.1, 0, 0, &mut q);
        }
        q.clear(); // discard first reassembly result

        buf.reset();

        // Same sequence should be accepted again after reset.
        let fragments2 = fragment_packet(1, &data, 4, 16).unwrap();
        let mut last = None;
        for frag in &fragments2 {
            last = buf.process_fragment(&frag.0, &frag.1, 0, 0, &mut q);
        }
        assert!(last.is_some());

        let reassembled = drain_first(&mut q);
        assert_eq!(reassembled, data);
    }

    #[test]
    fn test_copy_to() {
        // Directly test the copy_to method on ReassemblyData.
        let mut rd = ReassemblyData::new_preallocated(4, 8);
        rd.reset(0, 0, 0, 3);

        rd.store_fragment(0, &[1, 2, 3, 4]);
        rd.store_fragment(1, &[5, 6]);
        rd.store_fragment(2, &[7, 8, 9]);
        rd.mark_fragment(0);
        rd.mark_fragment(1);
        rd.mark_fragment(2);
        rd.num_fragments_received = 3;

        let mut dest = vec![0u8; 32];
        let written = rd.copy_to(&mut dest);
        assert_eq!(written, 9);
        assert_eq!(&dest[..9], &[1, 2, 3, 4, 5, 6, 7, 8, 9]);
    }
}
