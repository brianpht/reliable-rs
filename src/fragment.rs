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
//! buffer indexed by `sequence & (capacity - 1)`. When the last fragment for a
//! sequence arrives, the entry is removed from the buffer and the fragments are
//! concatenated in order to produce the original payload.
//!
//! Duplicate fragments are silently dropped (detected via a `[u32; 8]` bit
//! mask - one bit per fragment ID, supporting up to 256 fragments).
//! Out-of-order arrival is handled transparently.

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
    /// Write fragment header to a fixed-size buffer slice (allocation-free)
    ///
    /// Returns the number of bytes written, or None if buffer is too small
    #[allow(dead_code)]
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

    /// Write fragment header to buffer
    #[inline]
    pub fn write(&self, buffer: &mut Vec<u8>) -> usize {
        // Prefix byte with fragment flag set (bit 0 = 1)
        buffer.push(1);
        buffer.extend_from_slice(&self.sequence.to_le_bytes());
        buffer.push(self.fragment_id);
        buffer.push(self.num_fragments);
        FRAGMENT_HEADER_BYTES
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

/// Data for reassembling a fragmented packet
#[derive(Clone, Default)]
pub(crate) struct ReassemblyData {
    /// Sequence number
    pub sequence: u16,
    /// ACK from first fragment
    pub ack: u16,
    /// ACK bits from first fragment
    pub ack_bits: u32,
    /// Total number of fragments
    pub num_fragments: u8,
    /// Number of fragments received
    pub num_fragments_received: u8,
    /// Bitmask of received fragments
    pub fragment_received: [u32; 8],
    /// Fragment data storage - each fragment stored separately
    pub fragments: Vec<Vec<u8>>,
}

impl ReassemblyData {
    /// Check if a fragment has been received
    pub fn has_fragment(&self, fragment_id: u8) -> bool {
        let index = (fragment_id / 32) as usize;
        let bit = fragment_id % 32;
        (self.fragment_received[index] & (1 << bit)) != 0
    }

    /// Mark a fragment as received
    pub fn mark_fragment(&mut self, fragment_id: u8) {
        let index = (fragment_id / 32) as usize;
        let bit = fragment_id % 32;
        self.fragment_received[index] |= 1 << bit;
    }

    /// Check if all fragments have been received
    pub fn is_complete(&self) -> bool {
        self.num_fragments_received == self.num_fragments
    }

    /// Reassemble all fragments into a single packet
    pub fn reassemble(&self) -> Vec<u8> {
        let mut result = Vec::new();
        for fragment in &self.fragments {
            result.extend_from_slice(fragment);
        }
        result
    }
}

/// Fragment reassembly buffer
pub(crate) struct FragmentReassemblyBuffer {
    buffer: SequenceBuffer<ReassemblyData>,
    max_fragments: usize,
}

impl FragmentReassemblyBuffer {
    /// Create a new fragment reassembly buffer
    pub fn new(size: usize, _fragment_size: usize, max_fragments: usize) -> Self {
        Self {
            buffer: SequenceBuffer::new(size),
            max_fragments,
        }
    }

    /// Process a received fragment
    ///
    /// Returns Some(packet_data) if the packet is complete
    pub fn process_fragment(
        &mut self,
        header: &FragmentHeader,
        fragment_data: &[u8],
        ack: u16,
        ack_bits: u32,
    ) -> Option<Vec<u8>> {
        // Validate fragment
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

        // Get or create reassembly entry
        let is_new = !self.buffer.exists(header.sequence);

        if is_new && self.buffer.insert(header.sequence).is_none() {
            log::warn!("Failed to insert reassembly entry");
            return None;
        }

        let entry = self.buffer.find_mut(header.sequence)?;

        if is_new {
            // Initialize new entry
            entry.sequence = header.sequence;
            entry.ack = ack;
            entry.ack_bits = ack_bits;
            entry.num_fragments = header.num_fragments;

            // Pre-allocate fragment storage
            entry.fragments = vec![Vec::new(); header.num_fragments as usize];
        } else {
            // Verify consistency
            if entry.num_fragments != header.num_fragments {
                log::warn!(
                    "Fragment count mismatch: {} vs {}",
                    entry.num_fragments,
                    header.num_fragments
                );
                return None;
            }
        }

        // Check for duplicate
        if entry.has_fragment(header.fragment_id) {
            log::debug!(
                "Duplicate fragment {} for sequence {}",
                header.fragment_id,
                header.sequence
            );
            return None;
        }

        // Store fragment data
        let idx = header.fragment_id as usize;
        entry.fragments[idx] = fragment_data.to_vec();

        // Mark fragment as received
        entry.mark_fragment(header.fragment_id);
        entry.num_fragments_received += 1;

        // Check if complete
        if entry.is_complete() {
            let result = self.buffer.remove(header.sequence)?.reassemble();
            Some(result)
        } else {
            None
        }
    }

    /// Reset the buffer
    pub fn reset(&mut self) {
        self.buffer.reset();
    }
}

/// Fragment a packet into multiple parts
pub fn fragment_packet(
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

    #[test]
    fn test_fragment_header_roundtrip() {
        let header = FragmentHeader {
            sequence: 12345,
            fragment_id: 5,
            num_fragments: 16,
        };

        let mut buffer = Vec::new();
        header.write(&mut buffer);

        let (parsed, size) = FragmentHeader::read(&buffer).unwrap();
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

        // Process fragments out of order
        let result0 = reassembly.process_fragment(&fragments[2].0, &fragments[2].1, 0, 0);
        assert!(result0.is_none());

        let result1 = reassembly.process_fragment(&fragments[0].0, &fragments[0].1, 0, 0);
        assert!(result1.is_none());

        let result2 = reassembly.process_fragment(&fragments[1].0, &fragments[1].1, 0, 0);
        assert!(result2.is_some());

        let reassembled = result2.unwrap();
        assert_eq!(reassembled, data);
    }

    #[test]
    fn test_reassembly_in_order() {
        let data = vec![1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let fragments = fragment_packet(42, &data, 4, 16).unwrap();

        let mut reassembly = FragmentReassemblyBuffer::new(64, 4, 16);

        // Process fragments in order
        let result0 = reassembly.process_fragment(&fragments[0].0, &fragments[0].1, 0, 0);
        assert!(result0.is_none());

        let result1 = reassembly.process_fragment(&fragments[1].0, &fragments[1].1, 0, 0);
        assert!(result1.is_none());

        let result2 = reassembly.process_fragment(&fragments[2].0, &fragments[2].1, 0, 0);
        assert!(result2.is_some());

        let reassembled = result2.unwrap();
        assert_eq!(reassembled, data);
    }

    #[test]
    fn test_duplicate_fragment() {
        let data = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
        let fragments = fragment_packet(42, &data, 4, 16).unwrap();

        let mut reassembly = FragmentReassemblyBuffer::new(64, 4, 16);

        // Process first fragment
        reassembly.process_fragment(&fragments[0].0, &fragments[0].1, 0, 0);

        // Duplicate should return None
        let duplicate = reassembly.process_fragment(&fragments[0].0, &fragments[0].1, 0, 0);
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

        // Process in reverse order
        for i in (0..fragments.len()).rev() {
            let result = reassembly.process_fragment(&fragments[i].0, &fragments[i].1, 0, 0);
            if i == 0 {
                assert!(result.is_some());
                assert_eq!(result.unwrap(), data);
            } else {
                assert!(result.is_none());
            }
        }
    }
}
