//! Packet header serialization and deserialization

/// Maximum size of a packet header in bytes
pub const MAX_PACKET_HEADER_BYTES: usize = 9;

/// Packet header structure
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketHeader {
    /// Sequence number of this packet
    pub sequence: u16,
    /// ACK sequence number
    pub ack: u16,
    /// ACK bits (bitfield of 32 previous sequences)
    pub ack_bits: u32,
}

impl PacketHeader {
    /// Create a new packet header
    pub fn new(sequence: u16, ack: u16, ack_bits: u32) -> Self {
        Self {
            sequence,
            ack,
            ack_bits,
        }
    }

    /// Write the packet header to a fixed-size buffer slice (allocation-free)
    ///
    /// Returns the number of bytes written, or None if buffer is too small
    /// Buffer must be at least MAX_PACKET_HEADER_BYTES (9 bytes)
    #[allow(dead_code)]
    #[inline]
    pub fn write_to_slice(&self, buffer: &mut [u8]) -> Option<usize> {
        if buffer.len() < MAX_PACKET_HEADER_BYTES {
            return None;
        }

        let mut pos = 0;

        // Calculate prefix byte
        let mut prefix_byte: u8 = 0;

        // Bits 1-4: which ack_bits bytes to include (if not all 0xFF)
        if (self.ack_bits & 0x000000FF) != 0x000000FF {
            prefix_byte |= 1 << 1;
        }
        if (self.ack_bits & 0x0000FF00) != 0x0000FF00 {
            prefix_byte |= 1 << 2;
        }
        if (self.ack_bits & 0x00FF0000) != 0x00FF0000 {
            prefix_byte |= 1 << 3;
        }
        if (self.ack_bits & 0xFF000000) != 0xFF000000 {
            prefix_byte |= 1 << 4;
        }

        // Bit 5: sequence difference fits in one byte
        let sequence_diff = self.sequence_diff();
        if sequence_diff <= 255 {
            prefix_byte |= 1 << 5;
        }

        // Write prefix byte
        buffer[pos] = prefix_byte;
        pos += 1;

        // Write sequence (always 2 bytes, little-endian)
        let seq_bytes = self.sequence.to_le_bytes();
        buffer[pos] = seq_bytes[0];
        buffer[pos + 1] = seq_bytes[1];
        pos += 2;

        // Write ack (1 or 2 bytes)
        if sequence_diff <= 255 {
            buffer[pos] = sequence_diff as u8;
            pos += 1;
        } else {
            let ack_bytes = self.ack.to_le_bytes();
            buffer[pos] = ack_bytes[0];
            buffer[pos + 1] = ack_bytes[1];
            pos += 2;
        }

        // Write ack_bits (0-4 bytes, only non-0xFF bytes)
        if (self.ack_bits & 0x000000FF) != 0x000000FF {
            buffer[pos] = (self.ack_bits & 0xFF) as u8;
            pos += 1;
        }
        if (self.ack_bits & 0x0000FF00) != 0x0000FF00 {
            buffer[pos] = ((self.ack_bits >> 8) & 0xFF) as u8;
            pos += 1;
        }
        if (self.ack_bits & 0x00FF0000) != 0x00FF0000 {
            buffer[pos] = ((self.ack_bits >> 16) & 0xFF) as u8;
            pos += 1;
        }
        if (self.ack_bits & 0xFF000000) != 0xFF000000 {
            buffer[pos] = ((self.ack_bits >> 24) & 0xFF) as u8;
            pos += 1;
        }

        Some(pos)
    }

    /// Write the packet header to a buffer
    ///
    /// Returns the number of bytes written
    #[inline]
    pub fn write(&self, buffer: &mut Vec<u8>) -> usize {
        let start_len = buffer.len();

        // Calculate prefix byte
        let mut prefix_byte: u8 = 0;

        // Bit 0 is reserved for fragment flag (0 = regular packet)

        // Bits 1-4: which ack_bits bytes to include (if not all 0xFF)
        if (self.ack_bits & 0x000000FF) != 0x000000FF {
            prefix_byte |= 1 << 1;
        }
        if (self.ack_bits & 0x0000FF00) != 0x0000FF00 {
            prefix_byte |= 1 << 2;
        }
        if (self.ack_bits & 0x00FF0000) != 0x00FF0000 {
            prefix_byte |= 1 << 3;
        }
        if (self.ack_bits & 0xFF000000) != 0xFF000000 {
            prefix_byte |= 1 << 4;
        }

        // Bit 5: sequence difference fits in one byte
        let sequence_diff = self.sequence_diff();
        if sequence_diff <= 255 {
            prefix_byte |= 1 << 5;
        }

        // Write prefix byte
        buffer.push(prefix_byte);

        // Write sequence (always 2 bytes, little-endian)
        buffer.extend_from_slice(&self.sequence.to_le_bytes());

        // Write ack (1 or 2 bytes)
        if sequence_diff <= 255 {
            buffer.push(sequence_diff as u8);
        } else {
            buffer.extend_from_slice(&self.ack.to_le_bytes());
        }

        // Write ack_bits (0-4 bytes, only non-0xFF bytes)
        if (self.ack_bits & 0x000000FF) != 0x000000FF {
            buffer.push((self.ack_bits & 0xFF) as u8);
        }
        if (self.ack_bits & 0x0000FF00) != 0x0000FF00 {
            buffer.push(((self.ack_bits >> 8) & 0xFF) as u8);
        }
        if (self.ack_bits & 0x00FF0000) != 0x00FF0000 {
            buffer.push(((self.ack_bits >> 16) & 0xFF) as u8);
        }
        if (self.ack_bits & 0xFF000000) != 0xFF000000 {
            buffer.push(((self.ack_bits >> 24) & 0xFF) as u8);
        }

        buffer.len() - start_len
    }

    /// Read a packet header from a buffer
    ///
    /// Returns the header and the number of bytes read, or None if invalid
    #[inline]
    pub fn read(data: &[u8]) -> Option<(Self, usize)> {
        if data.len() < 3 {
            return None;
        }

        let prefix_byte = data[0];

        // Check if this is a fragment (bit 0 set)
        if (prefix_byte & 1) != 0 {
            return None;
        }

        // Read sequence
        let sequence = u16::from_le_bytes([data[1], data[2]]);
        let mut pos = 3;

        // Read ack
        let ack = if (prefix_byte & (1 << 5)) != 0 {
            // Sequence difference encoding
            if data.len() < pos + 1 {
                return None;
            }
            let diff = data[pos] as u16;
            pos += 1;
            sequence.wrapping_sub(diff)
        } else {
            // Full ack encoding
            if data.len() < pos + 2 {
                return None;
            }
            let ack = u16::from_le_bytes([data[pos], data[pos + 1]]);
            pos += 2;
            ack
        };

        // Read ack_bits
        let mut ack_bits: u32 = 0xFFFFFFFF;

        if (prefix_byte & (1 << 1)) != 0 {
            if data.len() < pos + 1 {
                return None;
            }
            ack_bits = (ack_bits & 0xFFFFFF00) | (data[pos] as u32);
            pos += 1;
        }
        if (prefix_byte & (1 << 2)) != 0 {
            if data.len() < pos + 1 {
                return None;
            }
            ack_bits = (ack_bits & 0xFFFF00FF) | ((data[pos] as u32) << 8);
            pos += 1;
        }
        if (prefix_byte & (1 << 3)) != 0 {
            if data.len() < pos + 1 {
                return None;
            }
            ack_bits = (ack_bits & 0xFF00FFFF) | ((data[pos] as u32) << 16);
            pos += 1;
        }
        if (prefix_byte & (1 << 4)) != 0 {
            if data.len() < pos + 1 {
                return None;
            }
            ack_bits = (ack_bits & 0x00FFFFFF) | ((data[pos] as u32) << 24);
            pos += 1;
        }

        Some((Self { sequence, ack, ack_bits }, pos))
    }

    /// Calculate sequence difference for encoding
    fn sequence_diff(&self) -> i32 {
        let diff = self.sequence.wrapping_sub(self.ack) as i32;
        if diff < 0 {
            diff + 65536
        } else {
            diff
        }
    }
}

/// Check if a buffer contains a fragment packet
pub fn is_fragment_packet(data: &[u8]) -> bool {
    !data.is_empty() && (data[0] & 1) != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_header_roundtrip() {
        let header = PacketHeader::new(100, 98, 0xFFFFFFFF);

        let mut buffer = Vec::new();
        let written = header.write(&mut buffer);

        let (parsed, read) = PacketHeader::read(&buffer).unwrap();

        assert_eq!(written, read);
        assert_eq!(header, parsed);
    }

    #[test]
    fn test_header_with_partial_acks() {
        let header = PacketHeader::new(100, 98, 0b11110000_11001100_10101010_01010101);

        let mut buffer = Vec::new();
        header.write(&mut buffer);

        let (parsed, _) = PacketHeader::read(&buffer).unwrap();
        assert_eq!(header, parsed);
    }

    #[test]
    fn test_header_wrap_around() {
        // Test sequence wrap-around
        let header = PacketHeader::new(5, 65530, 0xFFFFFFFF);

        let mut buffer = Vec::new();
        header.write(&mut buffer);

        let (parsed, _) = PacketHeader::read(&buffer).unwrap();
        assert_eq!(header, parsed);
    }

    #[test]
    fn test_header_sequence_diff_encoding() {
        // Small difference (fits in 1 byte)
        let header1 = PacketHeader::new(100, 50, 0xFFFFFFFF);
        let mut buf1 = Vec::new();
        let size1 = header1.write(&mut buf1);

        // Large difference (needs 2 bytes)
        let header2 = PacketHeader::new(100, 60000, 0xFFFFFFFF);
        let mut buf2 = Vec::new();
        let size2 = header2.write(&mut buf2);

        // Small diff should produce smaller header
        assert!(size1 < size2);

        // Both should roundtrip correctly
        let (p1, _) = PacketHeader::read(&buf1).unwrap();
        let (p2, _) = PacketHeader::read(&buf2).unwrap();
        assert_eq!(header1, p1);
        assert_eq!(header2, p2);
    }

    #[test]
    fn test_invalid_header() {
        // Too short
        assert!(PacketHeader::read(&[]).is_none());
        assert!(PacketHeader::read(&[0]).is_none());
        assert!(PacketHeader::read(&[0, 0]).is_none());

        // Fragment flag set
        assert!(PacketHeader::read(&[1, 0, 0, 0, 0]).is_none());
    }

    #[test]
    fn test_is_fragment() {
        assert!(!is_fragment_packet(&[0, 1, 2, 3]));
        assert!(is_fragment_packet(&[1, 1, 2, 3]));
        assert!(!is_fragment_packet(&[]));
    }

    #[test]
    fn test_max_header_size() {
        // Worst case: full ack (2 bytes) + all ack_bits (4 bytes)
        let header = PacketHeader::new(0, 40000, 0x00000000);

        let mut buffer = Vec::new();
        let size = header.write(&mut buffer);

        assert!(size <= MAX_PACKET_HEADER_BYTES);
    }
}