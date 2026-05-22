# RELIABLE-RS

A deterministic, allocation-free, lock-free reliable UDP transport core for
real-time games and applications. Pure Rust implementation inspired by
[reliable](https://github.com/mas-bandwidth/reliable) by Glenn Fiedler.

## Overview

`reliable-rs` is a packet acknowledgment system built on top of UDP. It is
designed for situations where you need to know which UDP packets were received
by the remote peer - such as real-time multiplayer games, voice chat, or other
latency-sensitive applications - without sacrificing the low-latency
characteristics of UDP.

## Features

- **Packet acknowledgment** - selective ACK with 32-bit ACK bitfield window
- **Fragmentation and reassembly** - large packets split and reassembled transparently, even out of order
- **Network statistics** - RTT, packet loss, and sent/received/acked bandwidth (EMA-smoothed)
- **Duplicate detection** - silently drops duplicate packets within the receive window
- **Allocation-free hot path** - zero heap allocations during steady-state send/receive
- **Lock-free** - single-writer model, no mutex on the hot path
- **Pure Rust** - no unsafe code, no external runtime dependencies

## Installation

```toml
[dependencies]
reliable-rs = "0.2"
```

## Quick Start

```rust
use reliable_rs::{Endpoint, EndpointConfig};

let mut client = Endpoint::new(EndpointConfig::default(), 0.0);
let mut server = Endpoint::new(EndpointConfig::default(), 0.0);

// Client sends a packet
client.send_packet(b"Hello, Server!");

// Drain outgoing datagrams and hand them to the UDP layer (zero-alloc)
client.drain_outgoing(|sequence, packet_data| {
    // socket.send_to(packet_data, server_addr).unwrap();
    server.receive_packet(packet_data); // simulated here
});

// Server reads the reassembled payload (zero-alloc)
server.drain_incoming(|sequence, payload| {
    println!("seq={} payload={:?}", sequence, payload);
});

// Server responds; the response datagram carries a piggy-backed ACK
server.send_packet(b"Hello, Client!");
let mut response_buf: Option<Vec<u8>> = None;
server.drain_outgoing(|_, bytes| response_buf = Some(bytes.to_vec()));
client.receive_packet(response_buf.as_deref().unwrap());

// Client checks which of its packets were acknowledged
for &ack in client.get_acks() {
    println!("packet {} was acknowledged", ack);
}
client.clear_acks();
```

## How It Works

```text
+--------+   send_packet    +----------+   drain_outgoing(|seq, bytes| ...)     +--------+
|        | ---------------> |          | -------------------------------------> |        |
|  App   |                  | Endpoint |                                        |  UDP   |
|        | <--------------- |          | <------------------------------------- |  Net   |
+--------+  drain_incoming  +----+-----+   receive_packet                       +--------+
                                 |
                    +------------+-----------+
                    |            |           |
             SequenceBuffer  Fragment    PacketHeader
             (sent/recv)    Reassembly   encode/decode
```

### ACK Mechanism

Every outgoing datagram carries two ACK fields in the packet header:

| Field      | Description                                                     |
|------------|-----------------------------------------------------------------|
| `ack`      | Highest received sequence number                                |
| `ack_bits` | 32-bit sliding window; bit `i` set means `ack - i` was received |

When the remote endpoint receives a datagram it processes these fields and
marks the corresponding sent-packet entries as acknowledged.

### Sequence Numbers

Each packet is assigned a 16-bit sequence number that wraps at 65535 -> 0.
All comparisons use half-range arithmetic to handle wrap-around correctly:
`s1 > s2` iff `(s1 - s2) mod 65536 < 32768`.

### Fragmentation

Packets larger than `fragment_above` are split into fragments, each transmitted
as a separate UDP datagram. Fragment 0 also carries the ACK header. The receiver
accumulates fragments in a ring buffer and delivers the reassembled payload once
all fragments have arrived, regardless of order.

## Wire Format

### Regular packet

```text
Byte 0:    prefix  (bit0=0 regular, bit1-4 ack_bits presence, bit5 ack encoding)
Bytes 1-2: sequence (LE u16)
Byte  3:   ack_diff (u8, when bit5=1)  OR
Bytes 3-4: ack      (LE u16, when bit5=0)
Bytes n..: ack_bits bytes that are NOT 0xFF (0-4 bytes, LE)
```

Size range: 4-9 bytes. Bytes in `ack_bits` that equal `0xFF` (all received)
are omitted, reducing header overhead in the common case.

### Fragment packet

```text
Bytes 0-4: FragmentHeader (prefix=0x01, seq LE u16, fragment_id, num_fragments)
[Bytes 5+: PacketHeader - present only in fragment 0]
Bytes n+:  fragment payload
```

All integers are little-endian.

## Configuration

```rust
use reliable_rs::EndpointConfig;

let config = EndpointConfig {
    name: "client".to_string(),
    max_packet_size: 16 * 1024,              // Maximum payload size (bytes); must be <= max_fragments * fragment_size
    fragment_above: 1024,                    // Fragment packets larger than this; must be <= max_packet_size
    max_fragments: 16,                       // Max fragments per packet (max 255)
    fragment_size: 1024,                     // Fragment payload size (bytes)
    sent_packets_buffer_size: 256,           // Power-of-two, >= 32 - sent packet ring buffer (loss detection window)
    received_packets_buffer_size: 256,       // Power-of-two - received packet ring buffer (duplicate detection)
    fragment_reassembly_buffer_size: 64,     // Power-of-two - reassembly ring buffer slots
    outgoing_queue_size: 256,                // Power-of-two - outgoing datagram ring buffer
    incoming_queue_size: 256,                // Power-of-two - incoming payload ring buffer
    ack_buffer_size: 256,                    // >= 32 - ACK notification buffer (call clear_acks() once per tick)
    rtt_smoothing_factor: 0.0025,            // EMA factor for RTT (range 0.0-1.0)
    packet_loss_smoothing_factor: 0.1,       // EMA factor for packet loss (range 0.0-1.0)
    bandwidth_smoothing_factor: 0.1,         // EMA factor for bandwidth (range 0.0-1.0)
    packet_header_size: 28,                  // IP(20) + UDP(8) overhead assumed for bandwidth calc
};

// Validate before use - checks all constraints listed below.
// Endpoint::new() calls this automatically and panics on failure.
config.validate().expect("invalid config");
```

> **Constraints enforced by `EndpointConfig::validate` (and panicked on by `Endpoint::new`):**
>
> | Field | Constraint |
> |-------|-----------|
> | `max_packet_size` | `> 0` and `<= max_fragments * fragment_size` |
> | `fragment_above` | `<= max_packet_size` |
> | `fragment_size`, `max_fragments` | `> 0` |
> | `sent_packets_buffer_size` | power-of-two and `>= 32` |
> | `received_packets_buffer_size` | power-of-two |
> | `fragment_reassembly_buffer_size` | power-of-two |
> | `outgoing_queue_size`, `incoming_queue_size` | power-of-two |
> | `ack_buffer_size` | `>= 32` (one full ACK batch per `receive_packet` call = 32 entries) |
> | smoothing factors | in `[0.0, 1.0]` |
>
> Ring-buffer indices are computed as `seq & (capacity - 1)`, which requires power-of-two capacities.

## Network Statistics

Call `endpoint.update(current_time)` once per tick to refresh statistics.

```rust
// Update with current wall-clock time in seconds
endpoint.update(current_time_seconds);

// Round-trip time in milliseconds (EMA)
let rtt = endpoint.rtt();

// Packet loss percentage 0-100 (EMA over sent-packet ring buffer)
let loss = endpoint.packet_loss();

// Bandwidth in kbps: (sent, received, acknowledged)
let (sent_kbps, recv_kbps, acked_kbps) = endpoint.bandwidth();

// Raw event counters
let c = endpoint.counters();
println!(
    "sent={} recv={} acked={} stale={} invalid={}",
    c.packets_sent, c.packets_received, c.packets_acked,
    c.packets_stale, c.packets_invalid
);
println!(
    "too_large_send={} too_large_recv={} frags_sent={} frags_recv={} frags_invalid={}",
    c.packets_too_large_to_send, c.packets_too_large_to_receive,
    c.fragments_sent, c.fragments_received, c.fragments_invalid
);
```

## Performance

Benchmarks on a modern x86_64 system (indicative, varies by hardware):

| Operation                              | Time     | Throughput   |
|----------------------------------------|----------|--------------|
| 64-byte packet send + receive          | ~40 ns   | ~1.5 GiB/s   |
| 512-byte packet send + receive         | ~54 ns   | ~9.0 GiB/s   |
| 4 KB packet fragmented send + receive  | ~305 ns  | ~13 GiB/s    |
| PacketHeader write                     | ~3.1 ns  | -            |
| PacketHeader read                      | ~1.4 ns  | -            |
| SequenceBuffer 100 insert+find ops     | ~2.1 us  | -            |
| Full ACK roundtrip (32 packets)        | ~2.3 us  | -            |

> Measured after v0.2.0 zero-alloc refactor. All heap allocations eliminated
> from the send/receive hot path. Previous v0.1 baseline: ~79 ns (64B),
> 7.3 GiB/s (fragmented), ~2.9 us (roundtrip).

### Performance Design

The library is built with deterministic, low-latency performance as a core
requirement. Key decisions:

- **Allocation-free hot path** - all buffers preallocated at `Endpoint::new`;
  no heap activity during steady-state operation
- **Lock-free** - single-writer model; no `Mutex` on the send/receive path
- **Ring buffers with bitwise indexing** - O(1) insert and lookup using
  `seq & (capacity - 1)`; capacities are always powers of two
- **Variable-length packet headers** - 4-9 bytes depending on ACK state;
  `0xFF` ack_bits bytes are elided
- **Half-range sequence arithmetic** - correct wrap-around handling with
  `wrapping_add` / `wrapping_sub` and half-range comparison
- **Branch-predictable code** - fast path first; error handlers marked `#[cold]`
- **Little-endian wire format** - `to_le_bytes` / `from_le_bytes` everywhere;
  zero overhead on x86_64

**Performance targets:**

| Metric               | Target    | Status        |
|----------------------|-----------|---------------|
| Small packet latency | < 200 ns  | ~40 ns        |
| Steady-state allocs  | Zero      | Zero          |
| Hot path cache misses| Zero      | Zero          |

See [docs/performance_design.md](docs/performance_design.md) for the full
performance design document.

## Integration with UDP Sockets

`reliable-rs` is transport-agnostic and works with any UDP socket library:

```rust
use std::net::UdpSocket;
use reliable_rs::{Endpoint, EndpointConfig};

let socket = UdpSocket::bind("0.0.0.0:0").unwrap();
let mut endpoint = Endpoint::new(EndpointConfig::default(), 0.0);

// Sending
endpoint.send_packet(b"game state update");
endpoint.drain_outgoing(|_, packet_data| {
    socket.send_to(packet_data, "server:9000").unwrap();
});

// Receiving
let mut buf = [0u8; 2048];
let (len, _addr) = socket.recv_from(&mut buf).unwrap();
endpoint.receive_packet(&buf[..len]);

// Update once per frame
endpoint.update(current_time_seconds);
```

For secure connections consider pairing with
[netcode](https://github.com/mas-bandwidth/netcode).

## Source Layout

| File                    | Description                                          |
|-------------------------|------------------------------------------------------|
| `src/config.rs`         | `EndpointConfig` - capacities and tuning knobs       |
| `src/endpoint.rs`       | `Endpoint` - send/receive, ACK processing, stats     |
| `src/fragment.rs`       | Fragmentation, reassembly, `FragmentHeader` encoding |
| `src/packet.rs`         | `PacketHeader` variable-length wire encoding         |
| `src/packet_queue.rs`   | `PacketQueue` - preallocated slot ring buffer        |
| `src/sequence_buffer.rs`| Power-of-two ring buffer for packet tracking         |
| `src/utils.rs`          | Half-range sequence comparison, EMA helper           |
| `benches/throughput.rs` | Criterion benchmarks                                 |
| `tests/integration.rs`  | Integration tests                                    |
| `examples/`             | `basic`, `client_server`, `with_packet_loss`, `udp_loopback` |

## Build Commands

```sh
cargo build --release          # library + examples
cargo test --workspace         # unit + integration tests
cargo bench                    # criterion benchmarks
cargo clippy --workspace --lib --bins -- -D warnings
```

## License

BSD 3-Clause License. See [LICENSE](LICENSE) for details.

## Credits

Rust implementation inspired by [reliable](https://github.com/mas-bandwidth/reliable)
by Glenn Fiedler.

Related libraries by Glenn Fiedler:
- [netcode](https://github.com/mas-bandwidth/netcode) - secure UDP connections
- [yojimbo](https://github.com/mas-bandwidth/yojimbo) - game networking library
- [serialize](https://github.com/mas-bandwidth/serialize) - bitpacking serialization
