//! Endpoint configuration - preallocated capacities and protocol tuning knobs.
//!
//! [`EndpointConfig`] is built once at startup and handed to [`Endpoint::new`].
//! All buffer sizes are fixed at that point; the library never reallocates
//! during steady-state operation.
//!
//! ## Power-of-Two Requirement
//!
//! `sent_packets_buffer_size`, `received_packets_buffer_size`, and
//! `fragment_reassembly_buffer_size` **must** be powers of two. The ring-buffer
//! index is computed as `seq & (capacity - 1)`, which is only correct when
//! capacity is a power of two. [`EndpointConfig::validate`] enforces this and
//! returns an `Err` if any constraint is violated.
//!
//! ## Defaults
//!
//! [`EndpointConfig::default`] provides values suitable for a 1 Mbps game
//! connection with packets up to 16 KB:
//!
//! | Field                              | Default  |
//! |------------------------------------|----------|
//! | `max_packet_size`                  | 16384    |
//! | `fragment_above`                   | 1024     |
//! | `fragment_size`                    | 1024     |
//! | `max_fragments`                    | 16       |
//! | `sent_packets_buffer_size`         | 256      |
//! | `received_packets_buffer_size`     | 256      |
//! | `fragment_reassembly_buffer_size`  | 64       |
//! | `outgoing_queue_size`              | 256      |
//! | `incoming_queue_size`              | 256      |
//! | `ack_buffer_size`                  | 256      |
//! | `rtt_smoothing_factor`             | 0.0025   |
//! | `packet_loss_smoothing_factor`     | 0.1      |
//! | `bandwidth_smoothing_factor`       | 0.1      |
//! | `packet_header_size`               | 28       |

/// Configuration for a reliable UDP endpoint.
///
/// Create with [`EndpointConfig::default`] or [`EndpointConfig::with_name`],
/// then call [`EndpointConfig::validate`] before passing to [`Endpoint::new`]
/// if you have changed any fields.
#[derive(Debug, Clone)]
pub struct EndpointConfig {
    /// Human-readable label used in log messages (debugging only, not on wire).
    pub name: String,

    /// Maximum payload size in bytes that [`Endpoint::send_packet`] will accept.
    ///
    /// Packets larger than this are rejected with a counter increment. Must
    /// satisfy `max_packet_size <= max_fragments * fragment_size`.
    pub max_packet_size: usize,

    /// Payload size threshold above which a packet is split into fragments.
    ///
    /// Packets at or below this size are sent as a single datagram. Must be
    /// `<= max_packet_size`.
    pub fragment_above: usize,

    /// Maximum number of fragments a single logical packet may produce.
    ///
    /// Also caps `num_fragments` in [`FragmentHeader`] to 255.
    pub max_fragments: usize,

    /// Size of each fragment payload in bytes.
    ///
    /// The actual UDP datagram will be larger by the combined header sizes.
    pub fragment_size: usize,

    /// Capacity of the ACK sequence ring buffer (power-of-two).
    ///
    /// Not directly a ring buffer size, but used indirectly when generating
    /// the 32-bit ACK bitfield window.
    pub ack_buffer_size: usize,

    /// Number of slots in the sent-packet ring buffer (power-of-two).
    ///
    /// Larger values extend the loss-detection window at the cost of memory.
    pub sent_packets_buffer_size: usize,

    /// Number of slots in the received-packet ring buffer (power-of-two).
    ///
    /// Controls how far back duplicate detection reaches.
    pub received_packets_buffer_size: usize,

    /// Number of slots in the fragment reassembly ring buffer (power-of-two).
    ///
    /// Each slot can hold one in-flight fragmented packet. Increase this if
    /// many large packets may be in flight simultaneously.
    pub fragment_reassembly_buffer_size: usize,

    /// Exponential moving average factor for RTT (range: 0.0-1.0).
    ///
    /// Lower values produce a smoother estimate that reacts more slowly.
    /// Default 0.0025 is appropriate for 60 Hz tick rates.
    pub rtt_smoothing_factor: f32,

    /// Exponential moving average factor for packet loss (range: 0.0-1.0).
    ///
    /// Computed over a window of `sent_packets_buffer_size / 2` samples.
    pub packet_loss_smoothing_factor: f32,

    /// Exponential moving average factor for bandwidth estimates (range: 0.0-1.0).
    pub bandwidth_smoothing_factor: f32,

    /// Combined IP + UDP header size assumed when computing bandwidth figures.
    ///
    /// Default 28 = 20 bytes IPv4 + 8 bytes UDP. Adjust for IPv6 (48) or
    /// tunnelled transports.
    pub packet_header_size: usize,

    /// Number of slots in the outgoing packet ring buffer (power-of-two).
    ///
    /// Each slot holds one UDP datagram sized to [`EndpointConfig::max_datagram_size`].
    /// Increase if the application may queue many packets per tick before draining.
    pub outgoing_queue_size: usize,

    /// Number of slots in the incoming packet ring buffer (power-of-two).
    ///
    /// Each slot holds one reassembled logical payload sized to
    /// [`EndpointConfig::max_packet_size`]. Increase if many packets may
    /// arrive between [`Endpoint::drain_incoming`] calls.
    pub incoming_queue_size: usize,
}

impl Default for EndpointConfig {
    fn default() -> Self {
        Self {
            name: "endpoint".to_string(),
            max_packet_size: 16 * 1024,
            fragment_above: 1024,
            max_fragments: 16,
            fragment_size: 1024,
            ack_buffer_size: 256,
            sent_packets_buffer_size: 256,
            received_packets_buffer_size: 256,
            fragment_reassembly_buffer_size: 64,
            rtt_smoothing_factor: 0.0025,
            packet_loss_smoothing_factor: 0.1,
            bandwidth_smoothing_factor: 0.1,
            packet_header_size: 28, // 20 (IP) + 8 (UDP)
            outgoing_queue_size: 256,
            incoming_queue_size: 256,
        }
    }
}

impl EndpointConfig {
    /// Create a configuration with a custom name and all other fields at their defaults.
    pub fn with_name(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }

    /// Maximum size of a single outgoing UDP datagram in bytes.
    ///
    /// Computed as `fragment_size + FRAGMENT_HEADER_BYTES + MAX_PACKET_HEADER_BYTES`.
    /// This is the required `slot_capacity` for the outgoing [`PacketQueue`].
    pub fn max_datagram_size(&self) -> usize {
        self.fragment_size
            + crate::fragment::FRAGMENT_HEADER_BYTES
            + crate::packet::MAX_PACKET_HEADER_BYTES
    }

    /// Validate the configuration and return a descriptive error if it is invalid.
    ///
    /// Checks performed:
    /// - `max_packet_size > 0`
    /// - `fragment_size > 0`
    /// - `max_fragments > 0`
    /// - `fragment_above <= max_packet_size`
    /// - `max_packet_size <= max_fragments * fragment_size`
    /// - `sent_packets_buffer_size` is a positive power of two
    /// - `received_packets_buffer_size` is a positive power of two
    /// - `fragment_reassembly_buffer_size` is a positive power of two
    /// - all smoothing factors are in `[0.0, 1.0]`
    pub fn validate(&self) -> Result<(), String> {
        if self.max_packet_size == 0 {
            return Err("max_packet_size must be > 0".to_string());
        }

        if self.fragment_size == 0 {
            return Err("fragment_size must be > 0".to_string());
        }

        if self.max_fragments == 0 {
            return Err("max_fragments must be > 0".to_string());
        }

        if self.fragment_above > self.max_packet_size {
            return Err("fragment_above must be <= max_packet_size".to_string());
        }

        let max_fragmented_size = self.max_fragments * self.fragment_size;
        if self.max_packet_size > max_fragmented_size {
            return Err(format!(
                "max_packet_size ({}) exceeds max fragmented size ({})",
                self.max_packet_size, max_fragmented_size
            ));
        }

        if self.sent_packets_buffer_size == 0 || !self.sent_packets_buffer_size.is_power_of_two() {
            return Err("sent_packets_buffer_size must be a power of two".to_string());
        }

        if self.received_packets_buffer_size == 0
            || !self.received_packets_buffer_size.is_power_of_two()
        {
            return Err("received_packets_buffer_size must be a power of two".to_string());
        }

        if self.fragment_reassembly_buffer_size == 0
            || !self.fragment_reassembly_buffer_size.is_power_of_two()
        {
            return Err("fragment_reassembly_buffer_size must be a power of two".to_string());
        }

        if !(0.0..=1.0).contains(&self.rtt_smoothing_factor) {
            return Err("rtt_smoothing_factor must be between 0.0 and 1.0".to_string());
        }

        if !(0.0..=1.0).contains(&self.packet_loss_smoothing_factor) {
            return Err("packet_loss_smoothing_factor must be between 0.0 and 1.0".to_string());
        }

        if !(0.0..=1.0).contains(&self.bandwidth_smoothing_factor) {
            return Err("bandwidth_smoothing_factor must be between 0.0 and 1.0".to_string());
        }

        if self.outgoing_queue_size == 0 || !self.outgoing_queue_size.is_power_of_two() {
            return Err("outgoing_queue_size must be a positive power of two".to_string());
        }

        if self.incoming_queue_size == 0 || !self.incoming_queue_size.is_power_of_two() {
            return Err("incoming_queue_size must be a positive power of two".to_string());
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = EndpointConfig::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_config_with_name() {
        let config = EndpointConfig::with_name("test_endpoint");
        assert_eq!(config.name, "test_endpoint");
    }

    #[test]
    fn test_invalid_config() {
        let mut config = EndpointConfig::default();
        config.max_packet_size = 0;
        assert!(config.validate().is_err());
    }
}
