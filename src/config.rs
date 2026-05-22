//! Endpoint configuration - preallocated capacities and protocol tuning knobs.
//!
//! [`EndpointConfig`] is built once at startup and handed to [`Endpoint::new`].
//! All buffer sizes are fixed at that point; the library never reallocates
//! during steady-state operation.
//!
//! [`Endpoint::new`] calls [`EndpointConfig::validate`] automatically and panics
//! if any constraint is violated, so invalid configs are detected at construction
//! time rather than producing silent misbehavior later.
//!
//! ## Constraints
//!
//! | Field | Constraint |
//! |-------|-----------|
//! | `max_packet_size` | `> 0` and `<= max_fragments * fragment_size` |
//! | `fragment_above` | `<= max_packet_size` |
//! | `fragment_size`, `max_fragments` | `> 0` |
//! | `sent_packets_buffer_size` | power-of-two and `>= 32` |
//! | `received_packets_buffer_size` | power-of-two |
//! | `fragment_reassembly_buffer_size` | power-of-two |
//! | `outgoing_queue_size`, `incoming_queue_size` | power-of-two |
//! | `ack_buffer_size` | `>= 32` (one full ACK batch per `receive_packet` call) |
//! | smoothing factors | in `[0.0, 1.0]` |
//!
//! Ring-buffer indices are computed as `seq & (capacity - 1)`, which requires
//! power-of-two capacities. The `>= 32` minimums come from the ACK bitfield
//! window: each received packet can acknowledge up to 32 sequences at once.
//!
//! ## Defaults
//!
//! [`EndpointConfig::default`] provides values suitable for a 1 Mbps game
//! connection with packets up to 16 KB:
//!
//! | Field                              | Default  | Constraint                         |
//! |------------------------------------|----------|------------------------------------|
//! | `max_packet_size`                  | 16384    | `<= max_fragments * fragment_size` |
//! | `fragment_above`                   | 1024     | `<= max_packet_size`               |
//! | `fragment_size`                    | 1024     | `> 0`                              |
//! | `max_fragments`                    | 16       | `> 0`, max 255                     |
//! | `sent_packets_buffer_size`         | 256      | power-of-two, `>= 32`              |
//! | `received_packets_buffer_size`     | 256      | power-of-two                       |
//! | `fragment_reassembly_buffer_size`  | 64       | power-of-two                       |
//! | `outgoing_queue_size`              | 256      | power-of-two                       |
//! | `incoming_queue_size`              | 256      | power-of-two                       |
//! | `ack_buffer_size`                  | 256      | `>= 32`                            |
//! | `rtt_smoothing_factor`             | 0.0025   | `[0.0, 1.0]`                       |
//! | `packet_loss_smoothing_factor`     | 0.1      | `[0.0, 1.0]`                       |
//! | `bandwidth_smoothing_factor`       | 0.1      | `[0.0, 1.0]`                       |
//! | `packet_header_size`               | 28       | informational only                 |

/// Configuration for a reliable UDP endpoint.
///
/// Create with [`EndpointConfig::default`] or [`EndpointConfig::with_name`],
/// then optionally adjust fields. Pass to [`Endpoint::new`], which calls
/// [`EndpointConfig::validate`] automatically and panics with a descriptive
/// message if any constraint is violated.
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

    /// Capacity of the ACK notification buffer.
    ///
    /// Determines how many acknowledged sequence numbers [`Endpoint::get_acks`]
    /// can return per tick. Each call to [`Endpoint::process_acks`] (internal)
    /// can add up to 32 entries (one per bit in the ACK bitfield). Call
    /// [`Endpoint::clear_acks`] once per tick to prevent overflow.
    ///
    /// Must be > 0. Should be >= 32 to hold at least one full ACK batch.
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
    /// [`Endpoint::new`] calls this automatically. Call it explicitly only when
    /// you want to surface errors without panicking (e.g., when loading config
    /// from user input).
    ///
    /// Checks performed:
    /// - `max_packet_size > 0`
    /// - `fragment_size > 0`
    /// - `max_fragments > 0`
    /// - `fragment_above <= max_packet_size`
    /// - `max_packet_size <= max_fragments * fragment_size`
    /// - `sent_packets_buffer_size` is a positive power of two and `>= 32`
    /// - `ack_buffer_size >= 32`
    /// - `received_packets_buffer_size` is a positive power of two
    /// - `fragment_reassembly_buffer_size` is a positive power of two
    /// - `outgoing_queue_size` is a positive power of two
    /// - `incoming_queue_size` is a positive power of two
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

        if self.sent_packets_buffer_size < 32 {
            return Err(
                "sent_packets_buffer_size must be >= 32 (one full ACK batch per receive)"
                    .to_string(),
            );
        }

        if self.ack_buffer_size == 0 {
            return Err("ack_buffer_size must be > 0".to_string());
        }

        if self.ack_buffer_size < 32 {
            return Err(
                "ack_buffer_size must be >= 32 (one full ACK batch = 32 entries)".to_string(),
            );
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
