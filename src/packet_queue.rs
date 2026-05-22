//! Allocation-free packet queue for outgoing and incoming packet staging.
//!
//! [`PacketQueue`] is a fixed-capacity ring buffer where every slot owns a
//! `Box<[u8]>` preallocated once in [`PacketQueue::new`]. No heap activity
//! occurs on the hot path.
//!
//! ## Index calculation
//!
//! ```text
//! slot_index = ptr & (capacity - 1)
//! ```
//!
//! Capacity must be a power of two (enforced with a panic in `new`).
//!
//! ## Write pattern
//!
//! Callers use [`PacketQueue::write_slot`] which passes a `&mut [u8]` of the
//! preallocated buffer directly to a closure. The closure writes in-place and
//! returns the byte count. This eliminates any intermediate allocation.
//!
//! ## Two instantiations in `Endpoint`
//!
//! | Queue | `slot_capacity` |
//! |-------|----------------|
//! | `outgoing_queue` | `config.max_datagram_size()` (per-datagram) |
//! | `incoming_queue` | `config.max_packet_size` (reassembled payload) |

/// One slot in a [`PacketQueue`].
pub(crate) struct PacketSlot {
    /// Sequence number of the packet in this slot.
    pub(crate) sequence: u16,
    /// Number of valid bytes in `data`.
    pub(crate) len: usize,
    /// Preallocated byte buffer. Allocated once at queue init; never reallocated.
    pub(crate) data: Box<[u8]>,
}

/// Fixed-capacity ring buffer of preallocated packet slots.
///
/// `capacity` must be a power of two. Each slot holds a byte buffer of
/// `slot_capacity` bytes allocated once at construction time.
pub(crate) struct PacketQueue {
    slots: Box<[PacketSlot]>,
    /// Next write position (wrapping `usize`). Slot index = `head & (capacity - 1)`.
    head: usize,
    /// Next read position (wrapping `usize`). Slot index = `tail & (capacity - 1)`.
    tail: usize,
    count: usize,
    /// Cached `slots.len()`. Always a power of two.
    capacity: usize,
}

impl PacketQueue {
    /// Create a new queue with all buffers preallocated.
    ///
    /// # Panics
    ///
    /// Panics if `queue_size` is zero or not a power of two, or if
    /// `slot_capacity` is zero.
    pub(crate) fn new(queue_size: usize, slot_capacity: usize) -> Self {
        assert!(
            queue_size > 0 && queue_size.is_power_of_two(),
            "PacketQueue queue_size must be a positive power of two, got {}",
            queue_size
        );
        assert!(slot_capacity > 0, "PacketQueue slot_capacity must be > 0");

        let slots: Vec<PacketSlot> = (0..queue_size)
            .map(|_| PacketSlot {
                sequence: 0,
                len: 0,
                data: vec![0u8; slot_capacity].into_boxed_slice(),
            })
            .collect();

        Self {
            slots: slots.into_boxed_slice(),
            head: 0,
            tail: 0,
            count: 0,
            capacity: queue_size,
        }
    }

    /// Write a packet into the next available slot via an in-place closure.
    ///
    /// The closure receives `&mut [u8]` (the slot's preallocated buffer) and
    /// must return the number of bytes written. Returns `false` if the queue
    /// is full and the write is dropped.
    ///
    /// Hot path - allocation-free.
    #[inline]
    pub(crate) fn write_slot(&mut self, sequence: u16, f: impl FnOnce(&mut [u8]) -> usize) -> bool {
        if self.count >= self.capacity {
            return false;
        }
        let idx = self.head & (self.capacity - 1);
        let slot = &mut self.slots[idx];
        slot.sequence = sequence;
        slot.len = f(&mut slot.data);
        debug_assert!(
            slot.len <= slot.data.len(),
            "write_slot: closure wrote {} bytes but slot capacity is {}",
            slot.len,
            slot.data.len()
        );
        self.head = self.head.wrapping_add(1);
        self.count += 1;
        true
    }

    /// Push a packet by copying `data` into the next available slot.
    ///
    /// Returns `false` if the queue is full. Prefer [`write_slot`] when
    /// building the packet in-place to avoid an extra copy.
    ///
    /// Hot path - allocation-free.
    #[inline]
    pub(crate) fn push(&mut self, sequence: u16, data: &[u8]) -> bool {
        self.write_slot(sequence, |buf| {
            let len = data.len().min(buf.len());
            buf[..len].copy_from_slice(&data[..len]);
            len
        })
    }

    /// Drain all packets in FIFO order.
    ///
    /// Calls `f(sequence, &data[..len])` for each queued packet, then resets
    /// the queue to empty. Allocation-free.
    #[inline]
    pub(crate) fn drain(&mut self, mut f: impl FnMut(u16, &[u8])) {
        while self.count > 0 {
            let idx = self.tail & (self.capacity - 1);
            let slot = &self.slots[idx];
            f(slot.sequence, &slot.data[..slot.len]);
            self.tail = self.tail.wrapping_add(1);
            self.count -= 1;
        }
    }

    /// Reset to empty without touching any buffer contents.
    #[inline]
    pub(crate) fn clear(&mut self) {
        self.head = 0;
        self.tail = 0;
        self.count = 0;
    }

    /// Number of packets currently queued.
    #[allow(dead_code)]
    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.count
    }

    /// Returns `true` if no packets are queued.
    #[allow(dead_code)]
    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Returns `true` if the queue has no free slots.
    #[allow(dead_code)]
    #[inline]
    pub(crate) fn is_full(&self) -> bool {
        self.count >= self.capacity
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_push_drain() {
        let mut q = PacketQueue::new(4, 64);

        assert!(q.push(0, b"hello"));
        assert!(q.push(1, b"world"));

        let mut results: Vec<(u16, Vec<u8>)> = Vec::new();
        q.drain(|seq, data| results.push((seq, data.to_vec())));

        assert_eq!(results.len(), 2);
        assert_eq!(results[0], (0, b"hello".to_vec()));
        assert_eq!(results[1], (1, b"world".to_vec()));
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn test_write_slot_in_place() {
        let mut q = PacketQueue::new(4, 64);

        let ok = q.write_slot(42, |buf| {
            buf[0..5].copy_from_slice(b"hello");
            5
        });
        assert!(ok);
        assert_eq!(q.len(), 1);

        let mut got_seq = 0u16;
        let mut got_data = Vec::new();
        q.drain(|seq, data| {
            got_seq = seq;
            got_data.extend_from_slice(data);
        });

        assert_eq!(got_seq, 42);
        assert_eq!(&got_data, b"hello");
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn test_full_queue_rejects() {
        let mut q = PacketQueue::new(2, 16);

        assert!(q.push(0, b"a"));
        assert!(q.push(1, b"b"));
        assert!(!q.push(2, b"c")); // queue is full
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn test_write_slot_returns_false_when_full() {
        let mut q = PacketQueue::new(2, 16);
        assert!(q.write_slot(0, |buf| {
            buf[0] = 1;
            1
        }));
        assert!(q.write_slot(1, |buf| {
            buf[0] = 2;
            1
        }));
        assert!(!q.write_slot(2, |buf| {
            buf[0] = 3;
            1
        }));
    }

    #[test]
    fn test_clear_and_reuse() {
        let mut q = PacketQueue::new(4, 16);
        q.push(0, b"x");
        q.push(1, b"y");
        q.clear();
        assert_eq!(q.len(), 0);
        assert!(q.push(2, b"z"));
        let mut count = 0usize;
        q.drain(|seq, _| {
            assert_eq!(seq, 2);
            count += 1;
        });
        assert_eq!(count, 1);
    }

    #[test]
    fn test_fifo_order_preserved() {
        let mut q = PacketQueue::new(8, 16);
        for i in 0u16..8 {
            q.push(i, &[i as u8]);
        }
        let mut order = Vec::new();
        q.drain(|seq, _| order.push(seq));
        assert_eq!(order, (0u16..8).collect::<Vec<_>>());
    }

    #[test]
    fn test_head_tail_wraparound() {
        let mut q = PacketQueue::new(4, 16);
        // Fill and drain 3 times to force head/tail to wrap past usize limits
        for cycle in 0..3u16 {
            for i in 0..4u16 {
                assert!(q.push(cycle * 4 + i, &[i as u8]));
            }
            let mut count = 0usize;
            q.drain(|seq, data| {
                assert_eq!(seq, cycle * 4 + count as u16);
                assert_eq!(data[0], count as u8);
                count += 1;
            });
            assert_eq!(count, 4);
        }
    }

    #[test]
    fn test_is_empty_is_full() {
        let mut q = PacketQueue::new(2, 8);
        assert!(q.is_empty());
        assert!(!q.is_full());
        q.push(0, b"a");
        assert!(!q.is_empty());
        assert!(!q.is_full());
        q.push(1, b"b");
        assert!(!q.is_empty());
        assert!(q.is_full());
    }

    #[test]
    #[should_panic(expected = "positive power of two")]
    fn test_non_power_of_two_panics() {
        PacketQueue::new(3, 16);
    }

    #[test]
    #[should_panic(expected = "slot_capacity must be > 0")]
    fn test_zero_slot_capacity_panics() {
        PacketQueue::new(4, 0);
    }
}
