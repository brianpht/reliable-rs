//! # reliable-rs
//!
//! A deterministic, allocation-free, lock-free reliable UDP transport core for
//! real-time games and applications.
//!
//! ## Design Goals
//!
//! - **Deterministic** - no unbounded memory growth, no nondeterministic latency
//! - **Allocation-free hot path** - zero heap allocations during steady-state send/receive
//! - **Lock-free** - single-writer model, no mutex contention on the hot path
//! - **Cache-local** - power-of-two ring buffers with bitwise indexing, hot fields first
//!
//! ## Architecture
//!
//! ```text
//! +--------+   send_packet   +----------+   drain_outgoing(|seq, bytes| ...)   +--------+
//! |        | --------------> |          | ----------------------------------> |        |
//! |  App   |                 | Endpoint |                                     |  UDP   |
//! |        | <-------------- |          | <---------------------------------- |  Net   |
//! +--------+  drain_incoming +----+-----+   receive_packet                    +--------+
//!                                 |
//!                    +------------+-----------+
//!                    |            |           |
//!             SequenceBuffer  Fragment    PacketHeader
//!             (sent/recv)    Reassembly   encode/decode
//! ```
//!
//! ## Modules
//!
//! | Module           | Responsibility                                              |
//! |------------------|-------------------------------------------------------------|
//! | `config`         | Endpoint tuning knobs and preallocated buffer capacities    |
//! | `endpoint`       | Send/receive logic, ACK processing, RTT/loss estimation     |
//! | `fragment`       | Packet fragmentation and reassembly                         |
//! | `packet`         | Wire format: variable-length header encode/decode           |
//! | `sequence_buffer`| Power-of-two ring buffer for sent/received packet tracking  |
//! | `utils`          | Sequence number wrapping arithmetic helpers                 |
//!
//! ## Quick Start
//!
//! ```rust
//! use reliable_rs::{Endpoint, EndpointConfig};
//!
//! let mut client = Endpoint::new(EndpointConfig::default(), 0.0);
//! let mut server = Endpoint::new(EndpointConfig::default(), 0.0);
//!
//! // Client queues a packet for transmission
//! client.send_packet(b"Hello, Server!");
//!
//! // Hand outgoing wire bytes to the UDP layer (zero-alloc)
//! let mut outgoing: Vec<Vec<u8>> = Vec::new();
//! client.drain_outgoing(|_, bytes| outgoing.push(bytes.to_vec()));
//! for bytes in &outgoing {
//!     server.receive_packet(bytes);
//! }
//!
//! // Read the reassembled payload (zero-alloc)
//! let mut payload: Vec<u8> = Vec::new();
//! server.drain_incoming(|_, data| payload = data.to_vec());
//! assert_eq!(payload, b"Hello, Server!");
//!
//! // Server response carries a piggy-backed ACK for the client's packet
//! server.send_packet(b"Hello, Client!");
//! let mut response: Vec<Vec<u8>> = Vec::new();
//! server.drain_outgoing(|_, bytes| response.push(bytes.to_vec()));
//! for bytes in &response {
//!     client.receive_packet(bytes);
//! }
//!
//! // Client now knows its packet was acknowledged
//! let acks = client.get_acks();
//! assert!(acks.contains(&0)); // sequence 0 was acked
//! client.clear_acks();
//! ```

#![warn(missing_docs)]
#![warn(clippy::all)]

mod config;
mod endpoint;
mod fragment;
mod packet;
mod packet_queue;
mod sequence_buffer;
mod utils;

pub use config::EndpointConfig;
pub use endpoint::{Endpoint, EndpointCounters};
pub use packet::{MAX_PACKET_HEADER_BYTES, PacketHeader};
pub use utils::{sequence_greater_than, sequence_less_than};

/// Fragment header size in bytes (prefix + sequence + fragment_id + num_fragments).
pub const FRAGMENT_HEADER_BYTES: usize = fragment::FRAGMENT_HEADER_BYTES;

/// Error types returned by the library.
///
/// All variants map to specific protocol or validation failures. The
/// `FragmentError` and `PacketTooLarge` variants carry human-readable context
/// strings; these allocate only on the error path, never on the hot path.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Packet exceeds [`EndpointConfig::max_packet_size`].
    #[error("Packet too large: {size} bytes (max: {max})")]
    PacketTooLarge {
        /// Actual packet size in bytes.
        size: usize,
        /// Configured maximum in bytes.
        max: usize,
    },

    /// Packet header could not be decoded (truncated or has wrong flags).
    #[error("Invalid packet header")]
    InvalidHeader,

    /// Fragment reassembly could not complete.
    #[error("Fragment reassembly failed: {reason}")]
    FragmentError {
        /// Human-readable description of why reassembly failed.
        reason: String,
    },

    /// Received sequence number is too old to fit in the receive window.
    #[error("Stale packet sequence: {sequence}")]
    StalePacket {
        /// The rejected sequence number.
        sequence: u16,
    },
}

/// Convenience `Result` alias using this crate's [`Error`] type.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {

    #[test]
    fn test_library_version() {
        assert!(env!("CARGO_PKG_VERSION").starts_with("0.2"));
    }
}
