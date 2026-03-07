//! Client-server example with proper message handling

use std::collections::HashMap;
use reliable_rs::{Endpoint, EndpointConfig};

/// Simple message that can be sent/received
#[derive(Debug)]
enum Message {
    Ping { id: u32 },
    Pong { id: u32 },
    Data { content: String },
}

impl Message {
    fn serialize(&self) -> Vec<u8> {
        match self {
            Message::Ping { id } => {
                let mut data = vec![0u8]; // Type tag
                data.extend_from_slice(&id.to_le_bytes());
                data
            }
            Message::Pong { id } => {
                let mut data = vec![1u8];
                data.extend_from_slice(&id.to_le_bytes());
                data
            }
            Message::Data { content } => {
                let mut data = vec![2u8];
                let bytes = content.as_bytes();
                data.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                data.extend_from_slice(bytes);
                data
            }
        }
    }

    fn deserialize(data: &[u8]) -> Option<Self> {
        if data.is_empty() {
            return None;
        }

        match data[0] {
            0 if data.len() >= 5 => {
                let id = u32::from_le_bytes([data[1], data[2], data[3], data[4]]);
                Some(Message::Ping { id })
            }
            1 if data.len() >= 5 => {
                let id = u32::from_le_bytes([data[1], data[2], data[3], data[4]]);
                Some(Message::Pong { id })
            }
            2 if data.len() >= 5 => {
                let len = u32::from_le_bytes([data[1], data[2], data[3], data[4]]) as usize;
                if data.len() >= 5 + len {
                    let content = String::from_utf8_lossy(&data[5..5 + len]).to_string();
                    Some(Message::Data { content })
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

fn main() {
    println!("Reliable UDP - Client/Server Example\n");

    let config = EndpointConfig::default();
    let mut client = Endpoint::new(config.clone(), 0.0);
    let mut server = Endpoint::new(config, 0.0);

    // Track pending pings
    let mut pending_pings: HashMap<u32, f64> = HashMap::new();
    let mut ping_id = 0u32;

    let mut time = 0.0;
    let delta_time = 1.0 / 60.0;

    for frame in 0..300 {
        time += delta_time;

        // Client sends ping every 60 frames (1 second)
        if frame % 60 == 0 {
            let msg = Message::Ping { id: ping_id };
            println!("[{:.2}s] Client sending Ping #{}", time, ping_id);
            client.send_packet(&msg.serialize());
            pending_pings.insert(ping_id, time);
            ping_id += 1;
        }

        // Also send some data
        if frame % 30 == 0 {
            let msg = Message::Data {
                content: format!("Hello from frame {}", frame),
            };
            client.send_packet(&msg.serialize());
        }

        // Network: client -> server
        for (_seq, data) in client.take_outgoing_packets() {
            server.receive_packet(&data);
        }

        // Server processes messages
        for (_seq, data) in server.take_incoming_packets() {
            if let Some(msg) = Message::deserialize(&data) {
                match msg {
                    Message::Ping { id } => {
                        println!("[{:.2}s] Server received Ping #{}, sending Pong", time, id);
                        let response = Message::Pong { id };
                        server.send_packet(&response.serialize());
                    }
                    Message::Data { content } => {
                        println!("[{:.2}s] Server received data: {}", time, content);
                    }
                    _ => {}
                }
            }
        }

        // Network: server -> client
        for (_seq, data) in server.take_outgoing_packets() {
            client.receive_packet(&data);
        }

        // Client processes responses
        for (_seq, data) in client.take_incoming_packets() {
            if let Some(msg) = Message::deserialize(&data) {
                if let Message::Pong { id } = msg {
                    if let Some(send_time) = pending_pings.remove(&id) {
                        let rtt = (time - send_time) * 1000.0;
                        println!(
                            "[{:.2}s] Client received Pong #{}, RTT: {:.2}ms",
                            time, id, rtt
                        );
                    }
                }
            }
        }

        client.clear_acks();
        server.clear_acks();

        client.update(time);
        server.update(time);
    }

    println!("\n=== Statistics ===");
    println!("Client measured RTT: {:.2}ms", client.rtt());
    println!("Packets sent: {}", client.counters().packets_sent);
    println!("Packets acked: {}", client.counters().packets_acked);
}