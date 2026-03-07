//! Endpoint implementation

use std::collections::VecDeque;

use crate::config::EndpointConfig;
use crate::fragment::{
    fragment_packet, FragmentHeader, FragmentReassemblyBuffer, FRAGMENT_HEADER_BYTES,
};
use crate::packet::{is_fragment_packet, PacketHeader, MAX_PACKET_HEADER_BYTES};
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
    acks: Vec<u16>,
    counters: EndpointCounters,
    sent_packets: SequenceBuffer<SentPacketData>,
    received_packets: SequenceBuffer<ReceivedPacketData>,
    fragment_reassembly: FragmentReassemblyBuffer,
    outgoing_packets: VecDeque<(u16, Vec<u8>)>,
    incoming_packets: VecDeque<(u16, Vec<u8>)>,
}

impl Endpoint {
    /// Create a new endpoint with the given configuration
    pub fn new(config: EndpointConfig, time: f64) -> Self {
        Self {
            sent_packets: SequenceBuffer::new(config.sent_packets_buffer_size),
            received_packets: SequenceBuffer::new(config.received_packets_buffer_size),
            fragment_reassembly: FragmentReassemblyBuffer::new(
                config.fragment_reassembly_buffer_size,
                config.fragment_size,
                config.max_fragments,
            ),
            config,
            time,
            sequence: 0,
            rtt: 0.0,
            packet_loss: 0.0,
            sent_bandwidth_kbps: 0.0,
            received_bandwidth_kbps: 0.0,
            acked_bandwidth_kbps: 0.0,
            acks: Vec::new(),
            counters: EndpointCounters::default(),
            outgoing_packets: VecDeque::new(),
            incoming_packets: VecDeque::new(),
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
    /// The packet will be fragmented if necessary. Call `take_outgoing_packets()`
    /// to get the actual data to send over the network.
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

        let mut buffer = Vec::with_capacity(MAX_PACKET_HEADER_BYTES + data.len());
        header.write(&mut buffer);
        buffer.extend_from_slice(data);

        self.outgoing_packets.push_back((sequence, buffer));
    }

    fn send_fragmented_packet(&mut self, sequence: u16, data: &[u8], ack: u16, ack_bits: u32) {
        let fragments = match fragment_packet(
            sequence,
            data,
            self.config.fragment_size,
            self.config.max_fragments,
        ) {
            Some(f) => f,
            None => {
                log::error!("Failed to fragment packet");
                return;
            }
        };

        for (i, (frag_header, frag_data)) in fragments.into_iter().enumerate() {
            let mut buffer = Vec::with_capacity(
                FRAGMENT_HEADER_BYTES + MAX_PACKET_HEADER_BYTES + frag_data.len(),
            );

            // Write fragment header
            frag_header.write(&mut buffer);

            // For first fragment, include packet header for ack/ack_bits
            if i == 0 {
                let packet_header = PacketHeader::new(sequence, ack, ack_bits);
                packet_header.write(&mut buffer);
            }

            buffer.extend_from_slice(&frag_data);

            self.outgoing_packets.push_back((sequence, buffer));
            self.counters.fragments_sent += 1;
        }
    }

    /// Receive a packet from the network
    ///
    /// Call `take_incoming_packets()` to get the processed packet data.
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

fn receive_regular_packet(&mut self, data: &[u8]) {
    let (header, header_size) = match PacketHeader::read(data) {
        Some(h) => h,
        None => {
            log::warn!("Invalid packet header");
            self.counters.packets_invalid += 1;
            return;
        }
    };

    // Check if we can accept this sequence (not too old)
    if !self.received_packets.can_insert(header.sequence) {
        log::debug!("Stale packet: sequence {}", header.sequence);
        self.counters.packets_stale += 1;
        return;
    }

    // Check for duplicate packet
    if self.received_packets.exists(header.sequence) {
        log::debug!("Duplicate packet: sequence {}", header.sequence);
        self.counters.packets_stale += 1;
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

    // Extract payload
    let payload = &data[header_size..];
    if !payload.is_empty() {
        self.incoming_packets
            .push_back((header.sequence, payload.to_vec()));
    }
}

    fn receive_fragment(&mut self, data: &[u8]) {
        let (frag_header, frag_header_size) = match FragmentHeader::read(data) {
            Some(h) => h,
            None => {
                log::warn!("Invalid fragment header");
                self.counters.fragments_invalid += 1;
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
                log::warn!("Invalid packet header in first fragment");
                self.counters.fragments_invalid += 1;
                return;
            }
        }

        let fragment_data = &data[pos..];

        // Try to reassemble
        if let Some(reassembled) =
            self.fragment_reassembly
                .process_fragment(&frag_header, fragment_data, ack, ack_bits)
        {
            self.counters.packets_received += 1;

            // Track as received
            if let Some(recv_data) = self.received_packets.insert(frag_header.sequence) {
                recv_data.time = self.time;
                recv_data.packet_bytes =
                    (self.config.packet_header_size + reassembled.len()) as u32;
            }

            self.incoming_packets
                .push_back((frag_header.sequence, reassembled));
        }
    }

    fn process_acks(&mut self, ack: u16, ack_bits: u32) {
        for i in 0..32 {
            if (ack_bits & (1 << i)) != 0 {
                let ack_sequence = ack.wrapping_sub(i as u16);

                if let Some(sent_data) = self.sent_packets.find_mut(ack_sequence) {
                    if !sent_data.acked {
                        sent_data.acked = true;
                        self.acks.push(ack_sequence);
                        self.counters.packets_acked += 1;

                        // Update RTT
                        let rtt_sample = ((self.time - sent_data.time) * 1000.0) as f32;
                        if self.rtt == 0.0 {
                            self.rtt = rtt_sample;
                        } else {
                            self.rtt = smooth_value(
                                self.rtt,
                                rtt_sample,
                                self.config.rtt_smoothing_factor,
                            );
                        }
                    }
                }
            }
        }
    }

    /// Take outgoing packets to send over the network
    pub fn take_outgoing_packets(&mut self) -> Vec<(u16, Vec<u8>)> {
        self.outgoing_packets.drain(..).collect()
    }

    /// Take incoming packets that have been processed
    pub fn take_incoming_packets(&mut self) -> Vec<(u16, Vec<u8>)> {
        self.incoming_packets.drain(..).collect()
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
            if let Some(data) = self.sent_packets.find(seq) {
                if !data.acked {
                    num_dropped += 1;
                }
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
            if let Some(data) = self.sent_packets.find(seq) {
                if data.acked {
                    acked_bytes += data.packet_bytes as u64;
                    acked_start_time = acked_start_time.min(data.time);
                    acked_end_time = acked_end_time.max(data.time);
                }
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

    /// Get acknowledged packet sequences
    pub fn get_acks(&self) -> &[u16] {
        &self.acks
    }

    /// Clear acknowledged packet sequences
    pub fn clear_acks(&mut self) {
        self.acks.clear();
    }

    /// Reset the endpoint to initial state
    pub fn reset(&mut self) {
        self.sequence = 0;
        self.acks.clear();
        self.rtt = 0.0;
        self.packet_loss = 0.0;
        self.sent_bandwidth_kbps = 0.0;
        self.received_bandwidth_kbps = 0.0;
        self.acked_bandwidth_kbps = 0.0;
        self.counters = EndpointCounters::default();
        self.sent_packets.reset();
        self.received_packets.reset();
        self.fragment_reassembly.reset();
        self.outgoing_packets.clear();
        self.incoming_packets.clear();
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

        // Client sends packet
        let data = b"Hello, Server!";
        client.send_packet(data);

        // Get outgoing packets
        let outgoing = client.take_outgoing_packets();
        assert_eq!(outgoing.len(), 1);

        // Server receives
        server.receive_packet(&outgoing[0].1);

        // Check incoming
        let incoming = server.take_incoming_packets();
        assert_eq!(incoming.len(), 1);
        assert_eq!(&incoming[0].1, data);
    }

    #[test]
    fn test_ack_system() {
        let mut client = create_test_endpoint("client");
        let mut server = create_test_endpoint("server");

        // Client sends
        client.send_packet(b"ping");
        let client_packets = client.take_outgoing_packets();

        // Server receives and sends response
        server.receive_packet(&client_packets[0].1);
        server.send_packet(b"pong");
        let server_packets = server.take_outgoing_packets();

        // Client receives response (contains ack)
        client.receive_packet(&server_packets[0].1);

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

        let mut client = Endpoint::new(config.clone(), 0.0);
        let mut server = Endpoint::new(config, 0.0);

        // Send large packet
        let data = vec![42u8; 500];
        client.send_packet(&data);

        // Should be fragmented
        let outgoing = client.take_outgoing_packets();
        assert!(outgoing.len() > 1);
        assert_eq!(client.counters().fragments_sent, outgoing.len() as u64);

        // Server receives all fragments
        for (_, packet_data) in &outgoing {
            server.receive_packet(packet_data);
        }

        // Check reassembled packet
        let incoming = server.take_incoming_packets();
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0].1, data);
    }

    #[test]
    fn test_fragmentation_out_of_order() {
        let mut config = EndpointConfig::default();
        config.fragment_above = 100;
        config.fragment_size = 100;

        let mut client = Endpoint::new(config.clone(), 0.0);
        let mut server = Endpoint::new(config, 0.0);

        // Send large packet
        let data: Vec<u8> = (0..500).map(|i| (i % 256) as u8).collect();
        client.send_packet(&data);

        // Should be fragmented
        let mut outgoing = client.take_outgoing_packets();
        assert!(outgoing.len() > 1);

        // Reverse order
        outgoing.reverse();

        // Server receives all fragments out of order
        for (_, packet_data) in &outgoing {
            server.receive_packet(packet_data);
        }

        // Check reassembled packet
        let incoming = server.take_incoming_packets();
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0].1, data);
    }

    #[test]
    fn test_rtt_measurement() {
        let mut client = create_test_endpoint("client");
        let mut server = create_test_endpoint("server");

        // Initial RTT should be 0
        assert_eq!(client.rtt(), 0.0);

        // Simulate round trip
        client.send_packet(b"ping");
        let packets = client.take_outgoing_packets();

        // Advance time
        client.update(0.050); // 50ms
        server.update(0.050);

        server.receive_packet(&packets[0].1);
        server.send_packet(b"pong");
        let response = server.take_outgoing_packets();

        // More time passes
        client.update(0.100); // 100ms total
        client.receive_packet(&response[0].1);

        // RTT should be approximately 100ms
        assert!(client.rtt() > 0.0);
    }

    #[test]
    fn test_sequence_wrap_around() {
        let mut endpoint = create_test_endpoint("test");

        // Force sequence near wrap-around
        for _ in 0..65534 {
            endpoint.send_packet(b"x");
            endpoint.take_outgoing_packets(); // Drain
        }

        // Should handle wrap-around
        endpoint.send_packet(b"wrap1");
        endpoint.send_packet(b"wrap2");

        let packets = endpoint.take_outgoing_packets();
        assert_eq!(packets.len(), 2);
    }

    #[test]
    fn test_reset() {
        let mut endpoint = create_test_endpoint("test");

        endpoint.send_packet(b"data");
        endpoint.take_outgoing_packets();

        endpoint.reset();

        assert_eq!(endpoint.next_packet_sequence(), 0);
        assert_eq!(endpoint.rtt(), 0.0);
        assert_eq!(endpoint.counters().packets_sent, 0);
    }
}