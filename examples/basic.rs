//! Basic example of using reliable-rs

use reliable_rs::{Endpoint, EndpointConfig};

fn main() {
    env_logger::init();

    println!("Reliable UDP - Basic Example\n");

    // Create two endpoints
    let config = EndpointConfig::default();
    let mut client = Endpoint::new(config.clone(), 0.0);
    let mut server = Endpoint::new(config, 0.0);

    let mut time = 0.0;
    let delta_time = 1.0 / 60.0; // 60 FPS

    // Simulate 100 frames
    for frame in 0..100 {
        time += delta_time;

        // Client sends a packet every frame
        let message = format!("Frame {}", frame);
        client.send_packet(message.as_bytes());

        // Get packets to "send" over network
        for (seq, data) in client.take_outgoing_packets() {
            println!("Client sent packet {} ({} bytes)", seq, data.len());

            // In real code, send over UDP socket
            // For demo, pass directly to server
            server.receive_packet(&data);
        }

        // Server processes received packets
        for (seq, data) in server.take_incoming_packets() {
            let msg = String::from_utf8_lossy(&data);
            println!("Server received packet {}: {}", seq, msg);
        }

        // Server sends response
        server.send_packet(b"ACK");

        for (_seq, data) in server.take_outgoing_packets() {
            client.receive_packet(&data);
        }

        // Process client's incoming packets (ACKs)
        client.take_incoming_packets();

        // Check for acknowledged packets
        for ack in client.get_acks() {
            println!("Client: packet {} was acknowledged", ack);
        }
        client.clear_acks();

        // Update endpoints
        client.update(time);
        server.update(time);
    }

    // Print final statistics
    println!("\n=== Final Statistics ===");
    println!("Client RTT: {:.2}ms", client.rtt());
    println!("Client packet loss: {:.1}%", client.packet_loss());

    let (sent, recv, acked) = client.bandwidth();
    println!(
        "Client bandwidth - Sent: {:.1} kbps, Recv: {:.1} kbps, Acked: {:.1} kbps",
        sent, recv, acked
    );

    let counters = client.counters();
    println!("\nClient counters:");
    println!("  Packets sent: {}", counters.packets_sent);
    println!("  Packets received: {}", counters.packets_received);
    println!("  Packets acked: {}", counters.packets_acked);
}