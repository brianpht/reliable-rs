//! Endpoint configuration

/// Configuration for an endpoint
#[derive(Debug, Clone)]
pub struct EndpointConfig {
    /// Name of the endpoint (for debugging)
    pub name: String,

    /// Maximum packet size in bytes
    pub max_packet_size: usize,

    /// Packets larger than this will be fragmented
    pub fragment_above: usize,

    /// Maximum number of fragments per packet
    pub max_fragments: usize,

    /// Size of each fragment in bytes
    pub fragment_size: usize,

    /// Size of the ACK buffer
    pub ack_buffer_size: usize,

    /// Size of the sent packets buffer
    pub sent_packets_buffer_size: usize,

    /// Size of the received packets buffer
    pub received_packets_buffer_size: usize,

    /// Size of the fragment reassembly buffer
    pub fragment_reassembly_buffer_size: usize,

    /// RTT smoothing factor (0.0 - 1.0)
    pub rtt_smoothing_factor: f32,

    /// Packet loss smoothing factor (0.0 - 1.0)
    pub packet_loss_smoothing_factor: f32,

    /// Bandwidth smoothing factor (0.0 - 1.0)
    pub bandwidth_smoothing_factor: f32,

    /// Size of packet header (IP + UDP headers)
    pub packet_header_size: usize,
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
        }
    }
}

impl EndpointConfig {
    /// Create a new configuration with the given name
    pub fn with_name(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }

    /// Validate the configuration
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

        if self.sent_packets_buffer_size == 0 {
            return Err("sent_packets_buffer_size must be > 0".to_string());
        }

        if self.received_packets_buffer_size == 0 {
            return Err("received_packets_buffer_size must be > 0".to_string());
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