//! Example with simulated packet loss

use rand::RngExt;
use reliable_rs::{Endpoint, EndpointConfig};

fn main() {
    env_logger::init();

    println!("Reliable UDP - Packet Loss Simulation\n");

    let config = EndpointConfig::default();
    let mut client = Endpoint::new(config.clone(), 0.0);
    let mut server = Endpoint::new(config, 0.0);

    let mut rng = rand::rng();
    let packet_loss_percent = 20; // 20% packet loss

    let mut time = 0.0;
    let delta_time = 1.0 / 60.0;

    let mut total_sent = 0;
    let mut total_dropped = 0;

    for frame in 0..500 {
        time += delta_time;

        // Client sends
        client.send_packet(format!("Message {}", frame).as_bytes());

        // Simulate lossy network: client -> server
        for (_seq, data) in client.take_outgoing_packets() {
            total_sent += 1;
            if rng.random_range(0..100) >= packet_loss_percent {
                server.receive_packet(&data);
            } else {
                total_dropped += 1;
            }
        }

        // Server processes and responds
        for (seq, data) in server.take_incoming_packets() {
            if frame % 50 == 0 {
                println!(
                    "Server received packet {}: {}",
                    seq,
                    String::from_utf8_lossy(&data)
                );
            }
        }

        server.send_packet(b"Response");

        // Simulate lossy network: server -> client
        for (_seq, data) in server.take_outgoing_packets() {
            total_sent += 1;
            if rng.random_range(0..100) >= packet_loss_percent {
                client.receive_packet(&data);
            } else {
                total_dropped += 1;
            }
        }

        client.take_incoming_packets();
        client.clear_acks();

        // Update
        client.update(time);
        server.update(time);
    }

    // Statistics
    println!("\n=== Results ===");
    println!("Simulated packet loss: {}%", packet_loss_percent);
    println!(
        "Actual packet loss: {:.1}%",
        (total_dropped as f32 / total_sent as f32) * 100.0
    );
    println!("Measured client packet loss: {:.1}%", client.packet_loss());
    println!("Client RTT: {:.2}ms", client.rtt());

    let client_counters = client.counters();
    let server_counters = server.counters();

    println!("\nClient:");
    println!("  Sent: {}", client_counters.packets_sent);
    println!("  Received: {}", client_counters.packets_received);
    println!("  Acked: {}", client_counters.packets_acked);
    println!("  Stale: {}", client_counters.packets_stale);

    println!("\nServer:");
    println!("  Sent: {}", server_counters.packets_sent);
    println!("  Received: {}", server_counters.packets_received);
    println!("  Acked: {}", server_counters.packets_acked);
}