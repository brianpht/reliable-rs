//! # reliable-rs
//!
//! A pure Rust implementation of reliable UDP protocol for real-time
//! games and applications.
//!
//! ## Overview
//!
//! This library provides:
//! - Packet acknowledgment with selective ACKs
//! - Automatic packet fragmentation and reassembly
//! - RTT and packet loss estimation
//! - Bandwidth tracking
//!
//! ## Example
//!
//! ```rust
//! use reliable_rs::{Endpoint, EndpointConfig};
//!
//! let config = EndpointConfig::default();
//! let mut endpoint = Endpoint::new(config, 0.0);
//!
//! // Send a packet
//! endpoint.send_packet(b"Hello!");
//!
//! // Get packets to transmit
//! let outgoing = endpoint.take_outgoing_packets();
//! ```

# ![warn(missing_docs)]
# ![warn(clippy::all)]

mod config;
mod endpoint;
mod fragment;
mod packet;
mod sequence_buffer;
mod utils;

pub use config::EndpointConfig;
pub use endpoint::{Endpoint, EndpointCounters};
pub use packet::{PacketHeader, MAX_PACKET_HEADER_BYTES};
pub use utils::{sequence_greater_than, sequence_less_than};

/// Fragment header size in bytes
pub const FRAGMENT_HEADER_BYTES: usize = fragment::FRAGMENT_HEADER_BYTES;

/// Error types for the library
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Packet exceeds maximum allowed size
    #[error("Packet too large: {size} bytes (max: {max})")]
    PacketTooLarge {
        /// Actual packet size
        size: usize,
        /// Maximum allowed size
        max: usize,
    },

    /// Invalid packet header
    #[error("Invalid packet header")]
    InvalidHeader,

    /// Fragment reassembly failed
    #[error("Fragment reassembly failed: {reason}")]
    FragmentError {
        /// Reason for failure
        reason: String,
    },

    /// Packet sequence is stale
    #[error("Stale packet sequence: {sequence}")]
    StalePacket {
        /// The stale sequence number
        sequence: u16,
    },
}

/// Result type for the library
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {

    #[test]
    fn test_library_version() {
        assert!(env!("CARGO_PKG_VERSION").starts_with("0.1"));
    }
}