//! Integration tests for reliable-rs

use reliable_rs::{Endpoint, EndpointConfig};

#[test]
fn test_basic_communication() {
    let config = EndpointConfig::default();
    let mut client = Endpoint::new(config.clone(), 0.0);
    let mut server = Endpoint::new(config, 0.0);

    // Client sends
    client.send_packet(b"Hello, Server!");

    // Transfer
    let mut packet_count = 0usize;
    client.drain_outgoing(|_, data| {
        packet_count += 1;
        server.receive_packet(data);
    });
    assert_eq!(packet_count, 1);

    // Server receives
    let mut received: Vec<(u16, Vec<u8>)> = Vec::new();
    server.drain_incoming(|seq, data| received.push((seq, data.to_vec())));
    assert_eq!(received.len(), 1);
    assert_eq!(&received[0].1, b"Hello, Server!");
}

#[test]
fn test_bidirectional_communication() {
    let config = EndpointConfig::default();
    let mut client = Endpoint::new(config.clone(), 0.0);
    let mut server = Endpoint::new(config, 0.0);

    // Multiple rounds
    for round in 0..10 {
        // Client -> Server
        client.send_packet(format!("ping {}", round).as_bytes());
        client.drain_outgoing(|_, data| server.receive_packet(data));

        // Server -> Client
        server.send_packet(format!("pong {}", round).as_bytes());
        server.drain_outgoing(|_, data| client.receive_packet(data));

        // Verify
        let mut client_count = 0usize;
        client.drain_incoming(|_, _| client_count += 1);
        let mut server_count = 0usize;
        server.drain_incoming(|_, _| server_count += 1);

        assert_eq!(client_count, 1);
        assert_eq!(server_count, 1);

        client.clear_acks();
        server.clear_acks();
    }
}

#[test]
fn test_acknowledgment_system() {
    let config = EndpointConfig::default();
    let mut client = Endpoint::new(config.clone(), 0.0);
    let mut server = Endpoint::new(config, 0.0);

    // Send multiple packets
    for i in 0..10 {
        client.send_packet(&[i as u8; 32]);
    }

    // Transfer to server
    client.drain_outgoing(|_, data| server.receive_packet(data));
    server.drain_incoming(|_, _| {});

    // Server responds (this will ack all received packets)
    server.send_packet(b"response");

    // Transfer back
    server.drain_outgoing(|_, data| client.receive_packet(data));
    client.drain_incoming(|_, _| {});

    // Check acks
    let acks = client.get_acks();
    assert_eq!(acks.len(), 10);

    // Verify all sequences are acked
    for i in 0..10 {
        assert!(acks.contains(&(i as u16)));
    }
}

#[test]
fn test_large_packet_fragmentation() {
    let mut config = EndpointConfig::default();
    config.fragment_above = 100;
    config.fragment_size = 100;
    config.max_fragments = 64;
    config.max_packet_size = config.max_fragments * config.fragment_size; // 64 * 100 = 6400

    let mut client = Endpoint::new(config.clone(), 0.0);
    let mut server = Endpoint::new(config, 0.0);

    // Create large packet
    let large_data: Vec<u8> = (0..5000).map(|i| (i % 256) as u8).collect();

    // Send
    client.send_packet(&large_data);

    // Should be fragmented
    let mut frag_count = 0usize;
    client.drain_outgoing(|_, data| {
        frag_count += 1;
        server.receive_packet(data);
    });
    assert!(frag_count > 1);

    // Receive reassembled packet
    let mut received: Vec<(u16, Vec<u8>)> = Vec::new();
    server.drain_incoming(|seq, data| received.push((seq, data.to_vec())));
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].1, large_data);
}

#[test]
fn test_out_of_order_fragments() {
    let mut config = EndpointConfig::default();
    config.fragment_above = 50;
    config.fragment_size = 50;
    config.max_packet_size = config.max_fragments * config.fragment_size; // 16 * 50 = 800

    let mut client = Endpoint::new(config.clone(), 0.0);
    let mut server = Endpoint::new(config, 0.0);

    let data: Vec<u8> = (0..200).map(|i| i as u8).collect();
    client.send_packet(&data);

    // Collect so we can reverse - acceptable in test code (not hot path)
    let mut packets: Vec<(u16, Vec<u8>)> = Vec::new();
    client.drain_outgoing(|seq, d| packets.push((seq, d.to_vec())));

    // Reverse order
    packets.reverse();

    // Send out of order
    for (_, packet_data) in &packets {
        server.receive_packet(packet_data);
    }

    // Should still reassemble correctly
    let mut received: Vec<(u16, Vec<u8>)> = Vec::new();
    server.drain_incoming(|seq, d| received.push((seq, d.to_vec())));
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].1, data);
}

#[test]
fn test_duplicate_packet_handling() {
    let config = EndpointConfig::default();
    let mut client = Endpoint::new(config.clone(), 0.0);
    let mut server = Endpoint::new(config, 0.0);

    client.send_packet(b"test");
    let mut packets: Vec<(u16, Vec<u8>)> = Vec::new();
    client.drain_outgoing(|seq, d| packets.push((seq, d.to_vec())));

    // Send same packet multiple times
    for _ in 0..5 {
        server.receive_packet(&packets[0].1);
    }

    // Should only receive once
    let mut received_count = 0usize;
    server.drain_incoming(|_, _| received_count += 1);
    assert_eq!(received_count, 1);

    // Additional receives should be ignored (stale)
    assert!(server.counters().packets_stale > 0 || server.counters().packets_received == 1);
}

#[test]
fn test_statistics_tracking() {
    let config = EndpointConfig::default();
    let mut client = Endpoint::new(config.clone(), 0.0);
    let mut server = Endpoint::new(config, 0.0);

    let mut time = 0.0;

    for _ in 0..100 {
        time += 0.016; // ~60 FPS

        client.send_packet(b"ping");
        client.drain_outgoing(|_, data| server.receive_packet(data));
        server.drain_incoming(|_, _| {});

        server.send_packet(b"pong");
        server.drain_outgoing(|_, data| client.receive_packet(data));
        client.drain_incoming(|_, _| {});
        client.clear_acks();

        client.update(time);
        server.update(time);
    }

    // Verify counters
    assert_eq!(client.counters().packets_sent, 100);
    assert_eq!(server.counters().packets_sent, 100);
    assert!(client.counters().packets_acked > 0);

    // RTT should be measured (will be close to 0 in this test since no real delay)
    // In real scenarios with network delay, this would be > 0
}

#[test]
fn test_endpoint_reset() {
    let config = EndpointConfig::default();
    let mut endpoint = Endpoint::new(config, 0.0);

    // Send some packets
    for i in 0..50 {
        endpoint.send_packet(&[i; 32]);
    }
    endpoint.drain_outgoing(|_, _| {});

    // Verify state
    assert_eq!(endpoint.next_packet_sequence(), 50);
    assert_eq!(endpoint.counters().packets_sent, 50);

    // Reset
    endpoint.reset();

    // Verify reset state
    assert_eq!(endpoint.next_packet_sequence(), 0);
    assert_eq!(endpoint.counters().packets_sent, 0);
    assert_eq!(endpoint.rtt(), 0.0);
    assert_eq!(endpoint.packet_loss(), 0.0);
}

#[test]
fn test_sequence_wrap_around() {
    let config = EndpointConfig::default();
    let mut client = Endpoint::new(config.clone(), 0.0);
    let mut server = Endpoint::new(config, 0.0);

    // Send enough packets to wrap around u16
    for i in 0..65540u32 {
        client.send_packet(&(i as u32).to_le_bytes());

        // Only process some to avoid memory issues
        if i % 100 == 0 {
            client.drain_outgoing(|_, data| server.receive_packet(data));
            server.drain_incoming(|_, _| {});
        }
    }

    // Should handle wrap-around gracefully
    assert!(client.next_packet_sequence() < 100); // Wrapped around
}

#[test]
fn test_max_packet_size_enforcement() {
    let mut config = EndpointConfig::default();
    config.max_packet_size = 100;
    config.fragment_above = 100; // must be <= max_packet_size

    let mut endpoint = Endpoint::new(config, 0.0);

    // Try to send oversized packet
    let large_data = vec![0u8; 200];
    endpoint.send_packet(&large_data);

    // Should be rejected
    let mut count = 0usize;
    endpoint.drain_outgoing(|_, _| count += 1);
    assert_eq!(count, 0);
    assert_eq!(endpoint.counters().packets_too_large_to_send, 1);
}
