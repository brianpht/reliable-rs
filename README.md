# RELIABLE-RS

A high-performance, pure Rust implementation of a reliable UDP protocol, inspired by [reliable](https://github.com/mas-bandwidth/reliable) by Glenn Fiedler.

## Overview

`reliable-rs` is a packet acknowledgment system for UDP-based protocols. It's designed for situations where you need to know which UDP packets were received by the other side, such as real-time multiplayer games, voice chat, or other latency-sensitive applications.

## Features

- **Packet Acknowledgment**: Know exactly which packets were received
- **Fragmentation & Reassembly**: Automatically split and reassemble large packets
- **Network Statistics**: RTT, jitter, packet loss, and bandwidth estimation
- **Duplicate Detection**: Automatically filters duplicate packets
- **Pure Rust**: No unsafe code, no external runtime dependencies
- **High Performance**: Sub-microsecond packet processing, 7+ GiB/s throughput

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
reliable-rs = "0.1"
```

## Quick Start

```rust
use reliable_rs::{Endpoint, EndpointConfig};

// Create endpoints for client and server
let config = EndpointConfig::default();
let mut client = Endpoint::new(config.clone(), 0.0);
let mut server = Endpoint::new(config, 0.0);

// Client sends a packet
client.send_packet(b"Hello, Server!");

// Get packets to transmit over your UDP socket
let outgoing = client.take_outgoing_packets();
for (sequence, packet_data) in &outgoing {
    // send packet_data over UDP...
}

// Server receives the packet
server.receive_packet(&outgoing[0].1);

// Server processes incoming packets
for (sequence, payload) in server.take_incoming_packets() {
    println!("Received: {:?}", payload);
}

// Server sends a response (includes ACK)
server.send_packet(b"Hello, Client!");
let response = server.take_outgoing_packets();

// Client receives response and processes ACKs
client.receive_packet(&response[0].1);
for &ack in client.get_acks() {
    println!("Packet {} was acknowledged", ack);
}
client.clear_acks();
```

## How It Works

The protocol uses a simple but effective acknowledgment system:

1. **Sequence Numbers**: Each packet gets a 16-bit sequence number that wraps around
2. **ACK Field**: Every packet includes the highest received sequence number
3. **ACK Bits**: A 32-bit bitfield acknowledges up to 32 previous packets
4. **Fragmentation**: Packets exceeding `fragment_above` are split into smaller fragments
5. **Reassembly**: Fragments are automatically reassembled, even if received out of order

## Configuration

```rust
let config = EndpointConfig {
    name: "client".to_string(),
    max_packet_size: 16 * 1024,             // Maximum packet size (16 KB)
    fragment_above: 1024,                    // Fragment packets larger than this
    max_fragments: 16,                       // Maximum fragments per packet
    fragment_size: 1024,                     // Size of each fragment
    sent_packets_buffer_size: 256,           // Buffer for tracking sent packets
    received_packets_buffer_size: 256,       // Buffer for tracking received packets
    fragment_reassembly_buffer_size: 64,     // Buffer for reassembling fragments
    rtt_smoothing_factor: 0.0025,            // RTT exponential smoothing
    packet_loss_smoothing_factor: 0.1,       // Packet loss smoothing
    bandwidth_smoothing_factor: 0.1,         // Bandwidth smoothing
    packet_header_size: 28,                  // IP + UDP header overhead
};
```

## Network Statistics

```rust
// Update endpoint each frame with current time
endpoint.update(current_time);

// Round-trip time in milliseconds
let rtt = endpoint.rtt();

// Packet loss percentage (0-100)
let loss = endpoint.packet_loss();

// Bandwidth in kbps (sent, received, acknowledged)
let (sent_kbps, recv_kbps, acked_kbps) = endpoint.bandwidth();

// Detailed counters
let counters = endpoint.counters();
println!("Sent: {}, Received: {}, Acked: {}", 
    counters.packets_sent,
    counters.packets_received, 
    counters.packets_acked);
```

## Performance

Benchmarks on a modern x86_64 system:

| Operation | Time | Throughput |
|-----------|------|------------|
| 64 byte packet | ~79 ns | 775 MiB/s |
| 512 byte packet | ~88 ns | 5.4 GiB/s |
| 4 KB fragmented | ~524 ns | 7.3 GiB/s |
| Header write | ~2.9 ns | - |
| Header read | ~1.3 ns | - |
| Sequence buffer (100 ops) | ~4.6 µs | - |
| Full ACK roundtrip (32 packets) | ~2.9 µs | - |

## Performance Design

This library is built with deterministic, low-latency performance as a core design principle. Key architectural decisions include:

- **Allocation-free hot path**: Zero heap allocations during send/receive operations
- **Lock-free design**: Single-writer model with no mutex contention
- **Ring buffers with bitwise indexing**: O(1) operations using power-of-two sizes
- **Cache-oriented data structures**: Hot fields aligned to minimize cache misses
- **Branch-predictable code**: Fast path first, error handlers marked `#[cold]`
- **Little-endian wire format**: Zero-overhead on x86_64 systems
- **Sequence arithmetic**: Proper wrapping arithmetic with half-range comparison

**Performance Targets:**
- Small packet latency: < 200ns ✅ (achieved ~79ns)
- Steady-state allocations: Zero
- Cache misses: None in hot path

For detailed performance design principles, architecture decisions, and optimization guidelines, see [docs/performance_design.md](docs/performance_design.md).

## Integration with Network Libraries

`reliable-rs` is transport-agnostic. It works with any UDP socket implementation:

```rust
use std::net::UdpSocket;

let socket = UdpSocket::bind("0.0.0.0:0").unwrap();
let mut endpoint = Endpoint::new(EndpointConfig::default(), 0.0);

// Sending
endpoint.send_packet(b"game state update");
for (_, packet_data) in endpoint.take_outgoing_packets() {
    socket.send_to(&packet_data, "server:9000").unwrap();
}

// Receiving
let mut buf = [0u8; 2048];
let (len, _addr) = socket.recv_from(&mut buf).unwrap();
endpoint.receive_packet(&buf[..len]);
```

For secure connections, consider using [netcode](https://github.com/mas-bandwidth/netcode) which pairs well with this library.

## License

BSD 3-Clause License. See [LICENSE](LICENSE) for details.

## Credits

This is a Rust implementation inspired by [reliable](https://github.com/mas-bandwidth/reliable) by Glenn Fiedler.

Other related libraries by Glenn Fiedler:
- [netcode](https://github.com/mas-bandwidth/netcode) - Secure UDP connections
- [yojimbo](https://github.com/mas-bandwidth/yojimbo) - Game networking library
- [serialize](https://github.com/mas-bandwidth/serialize) - Bitpacking serialization