//! Reliable UDP endpoint - the central object for sending and receiving packets.
//!
//! ## Lifecycle
//!
//! ```text
//! Endpoint::new(config, t0)
//!     |
//!     +-- send_packet(payload)        // queue one logical packet
//!     |       |-- fragment if payload > config.fragment_above
//!     |       +-- push to outgoing_queue (preallocated ring)
//!     |
//!     +-- drain_outgoing(|seq, bytes| ...)  // hand datagrams to UDP layer (zero-alloc)
//!     |
//!     +-- receive_packet(wire_bytes)  // feed a single UDP datagram
//!     |       |-- regular: decode PacketHeader, track recv, process ACKs
//!     |       +-- fragment: decode FragmentHeader, reassemble, track recv
//!     |
//!     +-- drain_incoming(|seq, payload| ...)  // read reassembled payloads (zero-alloc)
//!     |
//!     +-- update(current_time)        // call once per tick to update stats
//!     |       |-- update_packet_loss()
//!     |       +-- update_bandwidth()
//!     |
//!     +-- get_acks() / clear_acks()   // inspect acknowledged sequences
//! ```
//!
//! ## ACK Mechanism
//!
//! Every outgoing datagram carries:
//! - `ack` - the highest received sequence number seen so far
//! - `ack_bits` - a 32-bit sliding window; bit `i` set means sequence `ack - i` was also received
//!
//! The receiver calls [`process_acks`](Endpoint) internally on each
//! incoming datagram. When a sent-packet entry is found unacked, it is
//! marked acked, recorded in the bounded `ack_buf`, and its RTT sample is folded
//! into the EMA estimate. If `ack_buf` is full, the ACK is dropped with
//! `log::debug!` - see ADR-003.
//!
//! ## Statistics
//!
//! All statistics are updated lazily in [`Endpoint::update`]:
//! - **RTT** - EMA over round-trip samples derived from ACK timestamps
//! - **Packet loss** - fraction of unacked entries in the older half of the
//!   sent-packet ring buffer (smoothed EMA)
//! - **Bandwidth** - sent / received / acked kbps computed from ring-buffer
//!   timestamps and byte counts (smoothed EMA)

use crate::config::EndpointConfig;
use crate::fragment::{FragmentHeader, FragmentReassemblyBuffer};
use crate::packet::{MAX_PACKET_HEADER_BYTES, PacketHeader, is_fragment_packet};
use crate::packet_queue::PacketQueue;
use crate::sequence_buffer::SequenceBuffer;
use crate::utils::smooth_value;

/// Sent packet tracking data
#[derive(Clone, Default)]
struct SentPacketData {
    time: f64,
    acked: bool,
    packet_bytes: u32,
}

/// Received packet tracking data
#[derive(Clone, Default)]
struct ReceivedPacketData {
    time: f64,
    packet_bytes: u32,
}

/// Endpoint statistics counters
#[derive(Debug, Clone, Default)]
pub struct EndpointCounters {
    /// Number of packets sent
    pub packets_sent: u64,
    /// Number of packets received
    pub packets_received: u64,
    /// Number of packets acknowledged
    pub packets_acked: u64,
    /// Number of stale packets received
    pub packets_stale: u64,
    /// Number of invalid packets received
    pub packets_invalid: u64,
    /// Number of packets too large to send
    pub packets_too_large_to_send: u64,
    /// Number of packets too large to receive
    pub packets_too_large_to_receive: u64,
    /// Number of fragments sent
    pub fragments_sent: u64,
    /// Number of fragments received
    pub fragments_received: u64,
    /// Number of invalid fragments received
    pub fragments_invalid: u64,
}

/// A reliable UDP endpoint
pub struct Endpoint {
    config: EndpointConfig,
    time: f64,
    sequence: u16,
    rtt: f32,
    packet_loss: f32,
    sent_bandwidth_kbps: f32,
    received_bandwidth_kbps: f32,
    acked_bandwidth_kbps: f32,
    /// Preallocated ACK notification buffer. Sized to `sent_packets_buffer_size`.
    /// See ADR-003 for drop policy when full.
    ack_buf: Box<[u16]>,
    ack_count: usize,
    counters: EndpointCounters,
    sent_packets: SequenceBuffer<SentPacketData>,
    received_packets: SequenceBuffer<ReceivedPacketData>,
    fragment_reassembly: FragmentReassemblyBuffer,
    /// Preallocated ring buffer for outgoing datagrams.
    /// Slot capacity = `config.max_datagram_size()`.
    outgoing_queue: PacketQueue,
    /// Preallocated ring buffer for received payloads (after reassembly).
    /// Slot capacity = `config.max_packet_size`.
    incoming_queue: PacketQueue,
}

impl Endpoint {
    /// Create a new endpoint with the given configuration
    ///
    /// # Panics
    ///
    /// Panics if `config` fails [`EndpointConfig::validate`]. Call
    /// `config.validate()` first to surface errors without panicking.
    pub fn new(config: EndpointConfig, time: f64) -> Self {
        config
            .validate()
            .unwrap_or_else(|e| panic!("Invalid EndpointConfig: {e}"));
        let ack_buf = vec![0u16; config.ack_buffer_size].into_boxed_slice();
        let outgoing_queue =
            PacketQueue::new(config.outgoing_queue_size, config.max_datagram_size());
        let incoming_queue = PacketQueue::new(config.incoming_queue_size, config.max_packet_size);
        Self {
            sent_packets: SequenceBuffer::new(config.sent_packets_buffer_size),
            received_packets: SequenceBuffer::new(config.received_packets_buffer_size),
            fragment_reassembly: FragmentReassemblyBuffer::new(
                config.fragment_reassembly_buffer_size,
                config.fragment_size,
                config.max_fragments,
            ),
            outgoing_queue,
            incoming_queue,
            ack_buf,
            ack_count: 0,
            config,
            time,
            sequence: 0,
            rtt: 0.0,
            packet_loss: 0.0,
            sent_bandwidth_kbps: 0.0,
            received_bandwidth_kbps: 0.0,
            acked_bandwidth_kbps: 0.0,
            counters: EndpointCounters::default(),
        }
    }

    /// Get the configuration
    pub fn config(&self) -> &EndpointConfig {
        &self.config
    }

    /// Get the next packet sequence number
    pub fn next_packet_sequence(&self) -> u16 {
        self.sequence
    }

    /// Send a packet
    ///
    /// The packet will be fragmented if necessary. Call [`drain_outgoing`](Endpoint::drain_outgoing)
    /// to hand the encoded datagrams to the UDP layer.
    pub fn send_packet(&mut self, data: &[u8]) {
        if data.len() > self.config.max_packet_size {
            log::warn!(
                "Packet too large: {} bytes (max: {})",
                data.len(),
                self.config.max_packet_size
            );
            self.counters.packets_too_large_to_send += 1;
            return;
        }

        let sequence = self.sequence;
        self.sequence = self.sequence.wrapping_add(1);

        let (ack, ack_bits) = self.received_packets.generate_ack_bits();

        // Track sent packet
        if let Some(sent_data) = self.sent_packets.insert(sequence) {
            sent_data.time = self.time;
            sent_data.packet_bytes = (self.config.packet_header_size + data.len()) as u32;
            sent_data.acked = false;
        }

        // Check if fragmentation is needed
        if data.len() > self.config.fragment_above {
            self.send_fragmented_packet(sequence, data, ack, ack_bits);
        } else {
            self.send_regular_packet(sequence, data, ack, ack_bits);
        }

        self.counters.packets_sent += 1;
    }

    fn send_regular_packet(&mut self, sequence: u16, data: &[u8], ack: u16, ack_bits: u32) {
        let header = PacketHeader::new(sequence, ack, ack_bits);
        self.outgoing_queue.write_slot(sequence, |buf| {
            let hdr_len = match header.write_to_slice(buf) {
                Some(n) => n,
                None => return 0,
            };
            let payload_len = data.len().min(buf.len().saturating_sub(hdr_len));
            buf[hdr_len..hdr_len + payload_len].copy_from_slice(&data[..payload_len]);
            hdr_len + payload_len
        });
    }

    fn send_fragmented_packet(&mut self, sequence: u16, data: &[u8], ack: u16, ack_bits: u32) {
        let fragment_size = self.config.fragment_size;
        let num_fragments = data.len().div_ceil(fragment_size);

        if num_fragments > self.config.max_fragments || num_fragments > 255 {
            log::error!(
                "send_fragmented_packet: {} fragments needed but max is {}",
                num_fragments,
                self.config.max_fragments.min(255)
            );
            return;
        }

        let num_fragments_u8 = num_fragments as u8;
        let packet_header = PacketHeader::new(sequence, ack, ack_bits);

        for (i, chunk) in data.chunks(fragment_size).enumerate() {
            let frag_header = FragmentHeader {
                sequence,
                fragment_id: i as u8,
                num_fragments: num_fragments_u8,
            };

            self.outgoing_queue.write_slot(sequence, |buf| {
                let mut pos = 0;

                // Write fragment header (always present)
                match frag_header.write_to_slice(buf) {
                    Some(n) => pos += n,
                    None => return 0,
                }

                // First fragment: include packet header for ACK piggyback
                if i == 0 {
                    match packet_header.write_to_slice(&mut buf[pos..]) {
                        Some(n) => pos += n,
                        None => return 0,
                    }
                }

                // Write fragment payload
                let chunk_len = chunk.len().min(buf.len().saturating_sub(pos));
                buf[pos..pos + chunk_len].copy_from_slice(&chunk[..chunk_len]);
                pos + chunk_len
            });

            self.counters.fragments_sent += 1;
        }
    }

    /// Receive a packet from the network
    ///
    /// Call [`drain_incoming`](Endpoint::drain_incoming) to read processed packet payloads.
    pub fn receive_packet(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }

        if data.len() > self.config.max_packet_size + MAX_PACKET_HEADER_BYTES {
            log::warn!("Received packet too large: {} bytes", data.len());
            self.counters.packets_too_large_to_receive += 1;
            return;
        }

        if is_fragment_packet(data) {
            self.receive_fragment(data);
        } else {
            self.receive_regular_packet(data);
        }
    }

    #[inline]
    fn receive_regular_packet(&mut self, data: &[u8]) {
        let (header, header_size) = match PacketHeader::read(data) {
            Some(h) => h,
            None => {
                self.handle_invalid_packet();
                return;
            }
        };

        // Check if we can accept this sequence (not too old)
        if !self.received_packets.can_insert(header.sequence) {
            self.handle_stale_packet(header.sequence);
            return;
        }

        // Check for duplicate packet
        if self.received_packets.exists(header.sequence) {
            self.handle_duplicate_packet(header.sequence);
            return;
        }

        self.counters.packets_received += 1;

        // Track received packet
        if let Some(recv_data) = self.received_packets.insert(header.sequence) {
            recv_data.time = self.time;
            recv_data.packet_bytes = (self.config.packet_header_size + data.len()) as u32;
        }

        // Process acknowledgments
        self.process_acks(header.ack, header.ack_bits);

        // Push payload into incoming queue - zero-alloc
        let payload = &data[header_size..];
        if !payload.is_empty() {
            self.incoming_queue.push(header.sequence, payload);
        }
    }

    #[cold]
    #[inline(never)]
    fn handle_invalid_packet(&mut self) {
        log::warn!("Invalid packet header");
        self.counters.packets_invalid += 1;
    }

    #[cold]
    #[inline(never)]
    fn handle_stale_packet(&mut self, sequence: u16) {
        log::debug!("Stale packet: sequence {}", sequence);
        self.counters.packets_stale += 1;
    }

    #[cold]
    #[inline(never)]
    fn handle_duplicate_packet(&mut self, sequence: u16) {
        log::debug!("Duplicate packet: sequence {}", sequence);
        self.counters.packets_stale += 1;
    }

    #[inline]
    fn receive_fragment(&mut self, data: &[u8]) {
        let (frag_header, frag_header_size) = match FragmentHeader::read(data) {
            Some(h) => h,
            None => {
                self.handle_invalid_fragment();
                return;
            }
        };

        self.counters.fragments_received += 1;

        let mut pos = frag_header_size;
        let mut ack = 0u16;
        let mut ack_bits = 0xFFFFFFFFu32;

        // First fragment contains packet header
        if frag_header.fragment_id == 0 {
            if let Some((packet_header, packet_header_size)) = PacketHeader::read(&data[pos..]) {
                ack = packet_header.ack;
                ack_bits = packet_header.ack_bits;
                pos += packet_header_size;

                // Process acks from first fragment
                self.process_acks(ack, ack_bits);
            } else {
                self.handle_invalid_fragment_packet_header();
                return;
            }
        }

        let fragment_data = &data[pos..];

        // Disjoint field borrow: fragment_reassembly and incoming_queue are separate fields.
        // Rust 2021 NLL allows concurrent mutable borrows of disjoint struct fields.
        // See ADR-002 for design rationale.
        if let Some(payload_len) = self.fragment_reassembly.process_fragment(
            &frag_header,
            fragment_data,
            ack,
            ack_bits,
            &mut self.incoming_queue,
        ) {
            self.counters.packets_received += 1;

            // Track as received
            if let Some(recv_data) = self.received_packets.insert(frag_header.sequence) {
                recv_data.time = self.time;
                recv_data.packet_bytes = (self.config.packet_header_size + payload_len) as u32;
            }
        }
    }

    #[cold]
    #[inline(never)]
    fn handle_invalid_fragment(&mut self) {
        log::warn!("Invalid fragment header");
        self.counters.fragments_invalid += 1;
    }

    #[cold]
    #[inline(never)]
    fn handle_invalid_fragment_packet_header(&mut self) {
        log::warn!("Invalid packet header in first fragment");
        self.counters.fragments_invalid += 1;
    }

    #[inline]
    fn process_acks(&mut self, ack: u16, ack_bits: u32) {
        for i in 0..32 {
            if (ack_bits & (1 << i)) != 0 {
                let ack_sequence = ack.wrapping_sub(i as u16);

                if let Some(sent_data) = self.sent_packets.find_mut(ack_sequence)
                    && !sent_data.acked
                {
                    sent_data.acked = true;

                    // Bounded ACK buffer - see ADR-003 for drop policy.
                    if self.ack_count < self.ack_buf.len() {
                        self.ack_buf[self.ack_count] = ack_sequence;
                        self.ack_count += 1;
                    } else {
                        log::debug!("ack_buf full, dropping ack {}", ack_sequence);
                    }

                    self.counters.packets_acked += 1;

                    // Update RTT
                    let rtt_sample = ((self.time - sent_data.time) * 1000.0) as f32;
                    if self.rtt == 0.0 {
                        self.rtt = rtt_sample;
                    } else {
                        self.rtt =
                            smooth_value(self.rtt, rtt_sample, self.config.rtt_smoothing_factor);
                    }
                }
            }
        }
    }

    /// Drain outgoing datagrams via zero-alloc closure.
    ///
    /// Calls `f(sequence, wire_bytes)` for each queued datagram, then resets the
    /// outgoing queue to empty. The `wire_bytes` slice is borrowed from a
    /// preallocated slot and is valid only for the duration of the closure call.
    ///
    /// Hot path - allocation-free.
    pub fn drain_outgoing(&mut self, f: impl FnMut(u16, &[u8])) {
        self.outgoing_queue.drain(f);
    }

    /// Drain incoming reassembled payloads via zero-alloc closure.
    ///
    /// Calls `f(sequence, payload)` for each received packet, then resets the
    /// incoming queue to empty. The `payload` slice is borrowed from a
    /// preallocated slot and is valid only for the duration of the closure call.
    ///
    /// Hot path - allocation-free.
    pub fn drain_incoming(&mut self, f: impl FnMut(u16, &[u8])) {
        self.incoming_queue.drain(f);
    }

    /// Take outgoing packets to send over the network.
    ///
    /// Allocates a `Vec` on every call. Prefer [`drain_outgoing`](Endpoint::drain_outgoing)
    /// to avoid allocation.
    #[deprecated(since = "0.2.0", note = "use drain_outgoing to avoid allocation")]
    pub fn take_outgoing_packets(&mut self) -> Vec<(u16, Vec<u8>)> {
        let mut out = Vec::new();
        self.drain_outgoing(|seq, data| out.push((seq, data.to_vec())));
        out
    }

    /// Take incoming packets that have been processed.
    ///
    /// Allocates a `Vec` on every call. Prefer [`drain_incoming`](Endpoint::drain_incoming)
    /// to avoid allocation.
    #[deprecated(since = "0.2.0", note = "use drain_incoming to avoid allocation")]
    pub fn take_incoming_packets(&mut self) -> Vec<(u16, Vec<u8>)> {
        let mut out = Vec::new();
        self.drain_incoming(|seq, data| out.push((seq, data.to_vec())));
        out
    }

    /// Update the endpoint state
    ///
    /// Call this regularly (e.g., every frame) with the current time
    pub fn update(&mut self, time: f64) {
        self.time = time;
        self.update_packet_loss();
        self.update_bandwidth();
    }

    fn update_packet_loss(&mut self) {
        let buffer_size = self.config.sent_packets_buffer_size;
        let num_samples = buffer_size / 2;

        if self.sent_packets.sequence() < num_samples as u16 {
            return;
        }

        let mut num_dropped = 0;
        let base = self
            .sent_packets
            .sequence()
            .wrapping_sub(buffer_size as u16)
            .wrapping_add(1);

        for i in 0..num_samples {
            let seq = base.wrapping_add(i as u16);
            if let Some(data) = self.sent_packets.find(seq)
                && !data.acked
            {
                num_dropped += 1;
            }
        }

        let packet_loss = (num_dropped as f32 / num_samples as f32) * 100.0;
        self.packet_loss = smooth_value(
            self.packet_loss,
            packet_loss,
            self.config.packet_loss_smoothing_factor,
        );
    }

    fn update_bandwidth(&mut self) {
        let buffer_size = self.config.sent_packets_buffer_size;

        // Calculate sent bandwidth
        let mut sent_bytes = 0u64;
        let mut sent_start_time = f64::MAX;
        let mut sent_end_time = 0.0f64;

        let base = self
            .sent_packets
            .sequence()
            .wrapping_sub(buffer_size as u16);

        for i in 0..buffer_size {
            let seq = base.wrapping_add(i as u16);
            if let Some(data) = self.sent_packets.find(seq) {
                sent_bytes += data.packet_bytes as u64;
                sent_start_time = sent_start_time.min(data.time);
                sent_end_time = sent_end_time.max(data.time);
            }
        }

        if sent_end_time > sent_start_time {
            let duration = sent_end_time - sent_start_time;
            let kbps = (sent_bytes as f64 * 8.0 / duration / 1000.0) as f32;
            self.sent_bandwidth_kbps = smooth_value(
                self.sent_bandwidth_kbps,
                kbps,
                self.config.bandwidth_smoothing_factor,
            );
        }

        // Calculate received bandwidth
        let mut recv_bytes = 0u64;
        let mut recv_start_time = f64::MAX;
        let mut recv_end_time = 0.0f64;

        let recv_base = self
            .received_packets
            .sequence()
            .wrapping_sub(self.config.received_packets_buffer_size as u16);

        for i in 0..self.config.received_packets_buffer_size {
            let seq = recv_base.wrapping_add(i as u16);
            if let Some(data) = self.received_packets.find(seq) {
                recv_bytes += data.packet_bytes as u64;
                recv_start_time = recv_start_time.min(data.time);
                recv_end_time = recv_end_time.max(data.time);
            }
        }

        if recv_end_time > recv_start_time {
            let duration = recv_end_time - recv_start_time;
            let kbps = (recv_bytes as f64 * 8.0 / duration / 1000.0) as f32;
            self.received_bandwidth_kbps = smooth_value(
                self.received_bandwidth_kbps,
                kbps,
                self.config.bandwidth_smoothing_factor,
            );
        }

        // Calculate acked bandwidth
        let mut acked_bytes = 0u64;
        let mut acked_start_time = f64::MAX;
        let mut acked_end_time = 0.0f64;

        for i in 0..buffer_size {
            let seq = base.wrapping_add(i as u16);
            if let Some(data) = self.sent_packets.find(seq)
                && data.acked
            {
                acked_bytes += data.packet_bytes as u64;
                acked_start_time = acked_start_time.min(data.time);
                acked_end_time = acked_end_time.max(data.time);
            }
        }

        if acked_end_time > acked_start_time {
            let duration = acked_end_time - acked_start_time;
            let kbps = (acked_bytes as f64 * 8.0 / duration / 1000.0) as f32;
            self.acked_bandwidth_kbps = smooth_value(
                self.acked_bandwidth_kbps,
                kbps,
                self.config.bandwidth_smoothing_factor,
            );
        }
    }

    /// Get acknowledged packet sequences.
    ///
    /// Returns a slice of the bounded ACK buffer. Call [`clear_acks`](Endpoint::clear_acks)
    /// once per tick to avoid buffer overflow (see ADR-003).
    pub fn get_acks(&self) -> &[u16] {
        &self.ack_buf[..self.ack_count]
    }

    /// Clear acknowledged packet sequences
    pub fn clear_acks(&mut self) {
        self.ack_count = 0;
    }

    /// Reset the endpoint to initial state
    pub fn reset(&mut self) {
        self.sequence = 0;
        self.ack_count = 0;
        self.rtt = 0.0;
        self.packet_loss = 0.0;
        self.sent_bandwidth_kbps = 0.0;
        self.received_bandwidth_kbps = 0.0;
        self.acked_bandwidth_kbps = 0.0;
        self.counters = EndpointCounters::default();
        self.sent_packets.reset();
        self.received_packets.reset();
        self.fragment_reassembly.reset();
        self.outgoing_queue.clear();
        self.incoming_queue.clear();
    }

    /// Get the current RTT estimate in milliseconds
    pub fn rtt(&self) -> f32 {
        self.rtt
    }

    /// Get the current packet loss estimate (0-100%)
    pub fn packet_loss(&self) -> f32 {
        self.packet_loss
    }

    /// Get bandwidth estimates in kbps
    ///
    /// Returns (sent, received, acked) bandwidth
    pub fn bandwidth(&self) -> (f32, f32, f32) {
        (
            self.sent_bandwidth_kbps,
            self.received_bandwidth_kbps,
            self.acked_bandwidth_kbps,
        )
    }

    /// Get the endpoint counters
    pub fn counters(&self) -> &EndpointCounters {
        &self.counters
    }

    /// Get the current time
    pub fn time(&self) -> f64 {
        self.time
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_endpoint(name: &str) -> Endpoint {
        let config = EndpointConfig::with_name(name);
        Endpoint::new(config, 0.0)
    }

    #[test]
    fn test_send_receive_basic() {
        let mut client = create_test_endpoint("client");
        let mut server = create_test_endpoint("server");

        let data = b"Hello, Server!";
        client.send_packet(data);

        let mut outgoing_count = 0usize;
        client.drain_outgoing(|_, wire| {
            outgoing_count += 1;
            server.receive_packet(wire);
        });
        assert_eq!(outgoing_count, 1);

        let mut incoming: Vec<(u16, Vec<u8>)> = Vec::new();
        server.drain_incoming(|seq, d| incoming.push((seq, d.to_vec())));
        assert_eq!(incoming.len(), 1);
        assert_eq!(&incoming[0].1, data);
    }

    #[test]
    fn test_ack_system() {
        let mut client = create_test_endpoint("client");
        let mut server = create_test_endpoint("server");

        // Client sends
        client.send_packet(b"ping");
        client.drain_outgoing(|_, data| server.receive_packet(data));

        // Server receives and sends response
        server.send_packet(b"pong");
        server.drain_outgoing(|_, data| client.receive_packet(data));

        // Check that client got an ack
        let acks = client.get_acks();
        assert_eq!(acks.len(), 1);
        assert_eq!(acks[0], 0);
    }

    #[test]
    fn test_fragmentation() {
        let mut config = EndpointConfig::default();
        config.fragment_above = 100;
        config.fragment_size = 100;
        config.max_packet_size = config.max_fragments * config.fragment_size; // 16 * 100 = 1600

        let mut client = Endpoint::new(config.clone(), 0.0);
        let mut server = Endpoint::new(config, 0.0);

        // Send large packet
        let data = vec![42u8; 500];
        client.send_packet(&data);

        // Should be fragmented
        let mut frag_count = 0usize;
        client.drain_outgoing(|_, wire| {
            frag_count += 1;
            server.receive_packet(wire);
        });
        assert!(frag_count > 1);
        assert_eq!(client.counters().fragments_sent, frag_count as u64);

        // Check reassembled packet
        let mut incoming: Vec<(u16, Vec<u8>)> = Vec::new();
        server.drain_incoming(|seq, d| incoming.push((seq, d.to_vec())));
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0].1, data);
    }

    #[test]
    fn test_fragmentation_out_of_order() {
        let mut config = EndpointConfig::default();
        config.fragment_above = 100;
        config.fragment_size = 100;
        config.max_packet_size = config.max_fragments * config.fragment_size; // 16 * 100 = 1600

        let mut client = Endpoint::new(config.clone(), 0.0);
        let mut server = Endpoint::new(config, 0.0);

        // Send large packet
        let data: Vec<u8> = (0..500).map(|i| (i % 256) as u8).collect();
        client.send_packet(&data);

        // Collect datagrams first so we can reverse order
        let mut outgoing: Vec<(u16, Vec<u8>)> = Vec::new();
        client.drain_outgoing(|seq, d| outgoing.push((seq, d.to_vec())));
        assert!(outgoing.len() > 1);

        // Reverse order
        outgoing.reverse();

        // Server receives all fragments out of order
        for (_, wire) in &outgoing {
            server.receive_packet(wire);
        }

        // Check reassembled packet
        let mut incoming: Vec<(u16, Vec<u8>)> = Vec::new();
        server.drain_incoming(|seq, d| incoming.push((seq, d.to_vec())));
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0].1, data);
    }

    #[test]
    fn test_rtt_measurement() {
        let mut client = create_test_endpoint("client");
        let mut server = create_test_endpoint("server");

        // Initial RTT should be 0
        assert_eq!(client.rtt(), 0.0);

        // Simulate round trip - collect ping wire bytes
        client.send_packet(b"ping");
        let mut ping_wire: Option<Vec<u8>> = None;
        client.drain_outgoing(|_, d| ping_wire = Some(d.to_vec()));

        // Advance time
        client.update(0.050); // 50ms
        server.update(0.050);

        server.receive_packet(ping_wire.as_deref().unwrap());
        server.send_packet(b"pong");
        let mut pong_wire: Option<Vec<u8>> = None;
        server.drain_outgoing(|_, d| pong_wire = Some(d.to_vec()));

        // More time passes
        client.update(0.100); // 100ms total
        client.receive_packet(pong_wire.as_deref().unwrap());

        // RTT should be approximately 100ms
        assert!(client.rtt() > 0.0);
    }

    #[test]
    fn test_sequence_wrap_around() {
        let mut endpoint = create_test_endpoint("test");

        // Force sequence near wrap-around
        for _ in 0..65534 {
            endpoint.send_packet(b"x");
            endpoint.drain_outgoing(|_, _| {}); // Drain
        }

        // Should handle wrap-around
        endpoint.send_packet(b"wrap1");
        endpoint.send_packet(b"wrap2");

        let mut count = 0usize;
        endpoint.drain_outgoing(|_, _| count += 1);
        assert_eq!(count, 2);
    }

    #[test]
    fn test_reset() {
        let mut endpoint = create_test_endpoint("test");

        endpoint.send_packet(b"data");
        endpoint.drain_outgoing(|_, _| {});

        endpoint.reset();

        assert_eq!(endpoint.next_packet_sequence(), 0);
        assert_eq!(endpoint.rtt(), 0.0);
        assert_eq!(endpoint.counters().packets_sent, 0);
    }
}
