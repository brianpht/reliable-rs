//! Sequence buffer implementation for tracking sent and received packets

use crate::utils::sequence_less_than;

/// Entry in the sequence buffer
#[derive(Clone)]
pub(crate) struct BufferEntry<T> {
    /// Sequence number for this entry
    pub sequence: u16,
    /// Data stored in this entry
    pub data: T,
}

/// A circular buffer indexed by sequence numbers
///
/// This buffer handles sequence number wrap-around and provides
/// efficient O(1) insertion and lookup.
pub(crate) struct SequenceBuffer<T: Clone + Default> {
    /// Current sequence number (next expected)
    sequence: u16,
    /// Buffer entries
    entries: Vec<Option<BufferEntry<T>>>,
}

impl<T: Clone + Default> SequenceBuffer<T> {
    /// Create a new sequence buffer with the given size
    /// 
    /// # Panics
    /// Panics if size is not a power of two (required for O(1) bitwise indexing)
    pub fn new(size: usize) -> Self {
        assert!(size > 0 && size.is_power_of_two(), "SequenceBuffer size must be a power of two");
        Self {
            sequence: 0,
            entries: vec![None; size],
        }
    }

    /// Get the current sequence number
    pub fn sequence(&self) -> u16 {
        self.sequence
    }

    /// Get the buffer size
    #[allow(dead_code)]
    pub fn size(&self) -> usize {
        self.entries.len()
    }

    /// Reset the buffer to initial state
    pub fn reset(&mut self) {
        self.sequence = 0;
        for entry in &mut self.entries {
            *entry = None;
        }
    }

    /// Check if a sequence number can be inserted
    #[inline]
    pub fn can_insert(&self, sequence: u16) -> bool {
        let num_entries = self.entries.len() as u16;
        !sequence_less_than(sequence, self.sequence.wrapping_sub(num_entries))
    }

    /// Insert an entry at the given sequence number
    ///
    /// Returns a mutable reference to the data if successful
    #[inline]
    pub fn insert(&mut self, sequence: u16) -> Option<&mut T> {
        if !self.can_insert(sequence) {
            return None;
        }

        // If this sequence is ahead of current, advance and clear old entries
        if crate::utils::sequence_greater_than(sequence.wrapping_add(1), self.sequence) {
            self.remove_range(self.sequence, sequence);
            self.sequence = sequence.wrapping_add(1);
        }

        let index = self.index(sequence);
        self.entries[index] = Some(BufferEntry {
            sequence,
            data: T::default(),
        });

        self.entries[index].as_mut().map(|e| &mut e.data)
    }

    /// Insert with existing data
    #[allow(dead_code)]
    pub fn insert_with(&mut self, sequence: u16, data: T) -> bool {
        if let Some(entry_data) = self.insert(sequence) {
            *entry_data = data;
            true
        } else {
            false
        }
    }

    /// Find an entry by sequence number
    #[inline]
    pub fn find(&self, sequence: u16) -> Option<&T> {
        let index = self.index(sequence);
        self.entries[index]
            .as_ref()
            .filter(|e| e.sequence == sequence)
            .map(|e| &e.data)
    }

    /// Find a mutable entry by sequence number
    #[inline]
    pub fn find_mut(&mut self, sequence: u16) -> Option<&mut T> {
        let index = self.index(sequence);
        self.entries[index]
            .as_mut()
            .filter(|e| e.sequence == sequence)
            .map(|e| &mut e.data)
    }

    /// Remove an entry by sequence number
    #[inline]
    pub fn remove(&mut self, sequence: u16) -> Option<T> {
        let index = self.index(sequence);
        if let Some(entry) = self.entries[index].take() {
            if entry.sequence == sequence {
                return Some(entry.data);
            }
            // Put it back if wrong sequence
            self.entries[index] = Some(entry);
        }
        None
    }

    /// Check if an entry exists at the given sequence
    #[inline]
    pub fn exists(&self, sequence: u16) -> bool {
        let index = self.index(sequence);
        self.entries[index]
            .as_ref()
            .map(|e| e.sequence == sequence)
            .unwrap_or(false)
    }

    /// Generate ACK and ACK bits based on received packets
    ///
    /// Returns (ack, ack_bits) where:
    /// - ack is the most recent received sequence
    /// - ack_bits is a bitfield of the 32 sequences before ack
    pub fn generate_ack_bits(&self) -> (u16, u32) {
        let ack = self.sequence.wrapping_sub(1);
        let mut ack_bits: u32 = 0;

        for i in 0..32u16 {
            let seq = ack.wrapping_sub(i);
            if self.exists(seq) {
                ack_bits |= 1 << i;
            }
        }

        (ack, ack_bits)
    }

    /// Iterate over all valid entries
    #[allow(dead_code)]
    pub fn iter(&self) -> impl Iterator<Item = (u16, &T)> {
        self.entries
            .iter()
            .filter_map(|e| e.as_ref().map(|entry| (entry.sequence, &entry.data)))
    }

    /// Iterate over entries in a sequence range
    #[allow(dead_code)]
    pub fn iter_range(&self, start: u16, end: u16) -> impl Iterator<Item = (u16, &T)> + '_ {
        let mut seq = start;
        std::iter::from_fn(move || {
            while !crate::utils::sequence_greater_than(seq, end) {
                let current = seq;
                seq = seq.wrapping_add(1);
                if let Some(data) = self.find(current) {
                    return Some((current, data));
                }
            }
            None
        })
    }

    /// Calculate index in the buffer for a sequence number
    /// Uses bitwise AND instead of modulo for O(1) performance
    /// REQUIRES: capacity must be power-of-two
    #[inline]
    fn index(&self, sequence: u16) -> usize {
        (sequence as usize) & (self.entries.len() - 1)
    }

    /// Remove entries in a range (exclusive of end)
    fn remove_range(&mut self, start: u16, end: u16) {
        let mut seq = start;
        while sequence_less_than(seq, end.wrapping_add(1)) {
            let index = self.index(seq);
            self.entries[index] = None;
            seq = seq.wrapping_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Default, Debug, PartialEq)]
    struct TestData {
        value: u32,
    }

    #[test]
    fn test_basic_operations() {
        let mut buffer: SequenceBuffer<TestData> = SequenceBuffer::new(256);

        // Insert
        let data = buffer.insert(0).unwrap();
        data.value = 42;

        // Find
        assert_eq!(buffer.find(0).unwrap().value, 42);
        assert!(buffer.find(1).is_none());

        // Exists
        assert!(buffer.exists(0));
        assert!(!buffer.exists(1));
    }

    #[test]
    fn test_sequence_wrap_around() {
        let mut buffer: SequenceBuffer<TestData> = SequenceBuffer::new(256);

        // Insert at wrap-around point
        buffer.insert(65535).unwrap().value = 1;
        buffer.insert(0).unwrap().value = 2;
        buffer.insert(1).unwrap().value = 3;

        assert_eq!(buffer.find(65535).unwrap().value, 1);
        assert_eq!(buffer.find(0).unwrap().value, 2);
        assert_eq!(buffer.find(1).unwrap().value, 3);
    }

    #[test]
    fn test_stale_sequence_rejected() {
        let mut buffer: SequenceBuffer<TestData> = SequenceBuffer::new(256);

        // Advance sequence
        for i in 0..300u16 {
            buffer.insert(i);
        }

        // Old sequence should be rejected
        assert!(!buffer.can_insert(0));
        assert!(buffer.insert(0).is_none());
    }

    #[test]
    fn test_generate_ack_bits() {
        let mut buffer: SequenceBuffer<TestData> = SequenceBuffer::new(256);

        buffer.insert(0);
        buffer.insert(1);
        buffer.insert(3); // Skip 2

        let (ack, ack_bits) = buffer.generate_ack_bits();
        assert_eq!(ack, 3);
        // Bits: 0=exists(3), 1=exists(2)=no, 2=exists(1), 3=exists(0)
        assert_eq!(ack_bits & 0b1111, 0b1101);
    }

    #[test]
    fn test_remove() {
        let mut buffer: SequenceBuffer<TestData> = SequenceBuffer::new(256);

        buffer.insert(5).unwrap().value = 100;
        assert!(buffer.exists(5));

        let removed = buffer.remove(5).unwrap();
        assert_eq!(removed.value, 100);
        assert!(!buffer.exists(5));
    }

    #[test]
    fn test_reset() {
        let mut buffer: SequenceBuffer<TestData> = SequenceBuffer::new(256);

        buffer.insert(0);
        buffer.insert(1);
        buffer.insert(2);

        buffer.reset();

        assert!(!buffer.exists(0));
        assert!(!buffer.exists(1));
        assert!(!buffer.exists(2));
        assert_eq!(buffer.sequence(), 0);
    }
}