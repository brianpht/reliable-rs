//! Throughput benchmarks for reliable-rs

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use reliable_rs::{Endpoint, EndpointConfig};
use std::hint::black_box;

fn benchmark_send_receive(c: &mut Criterion) {
    let mut group = c.benchmark_group("send_receive");

    // Small packets
    group.throughput(Throughput::Bytes(64));
    group.bench_function("64_bytes", |b| {
        let config = EndpointConfig::default();
        let mut client = Endpoint::new(config.clone(), 0.0);
        let mut server = Endpoint::new(config, 0.0);
        let data = vec![0u8; 64];

        b.iter(|| {
            client.send_packet(black_box(&data));
            client.drain_outgoing(|_, packet| server.receive_packet(black_box(packet)));
            let mut count = 0usize;
            server.drain_incoming(|_, _| count += 1);
            black_box(count)
        });
    });

    // Medium packets
    group.throughput(Throughput::Bytes(512));
    group.bench_function("512_bytes", |b| {
        let config = EndpointConfig::default();
        let mut client = Endpoint::new(config.clone(), 0.0);
        let mut server = Endpoint::new(config, 0.0);
        let data = vec![0u8; 512];

        b.iter(|| {
            client.send_packet(black_box(&data));
            client.drain_outgoing(|_, packet| server.receive_packet(black_box(packet)));
            let mut count = 0usize;
            server.drain_incoming(|_, _| count += 1);
            black_box(count)
        });
    });

    // Large packets (will be fragmented)
    group.throughput(Throughput::Bytes(4096));
    group.bench_function("4096_bytes_fragmented", |b| {
        let config = EndpointConfig::default();
        let mut client = Endpoint::new(config.clone(), 0.0);
        let mut server = Endpoint::new(config, 0.0);
        let data = vec![0u8; 4096];

        b.iter(|| {
            client.send_packet(black_box(&data));
            client.drain_outgoing(|_, packet| server.receive_packet(black_box(packet)));
            let mut count = 0usize;
            server.drain_incoming(|_, _| count += 1);
            black_box(count)
        });
    });

    group.finish();
}

fn benchmark_packet_header(c: &mut Criterion) {
    use reliable_rs::{MAX_PACKET_HEADER_BYTES, PacketHeader};

    let mut group = c.benchmark_group("packet_header");

    // Measures the hot-path write_to_slice (zero-alloc, stack buffer).
    group.bench_function("write", |b| {
        let header = PacketHeader::new(1000, 998, 0xFFFFFFFF);
        let mut buffer = [0u8; MAX_PACKET_HEADER_BYTES];

        b.iter(|| {
            let written = black_box(&header).write_to_slice(&mut buffer);
            black_box(written)
        });
    });

    group.bench_function("read", |b| {
        // Setup: encode once outside the measured loop.
        let header = PacketHeader::new(1000, 998, 0xFFFFFFFF);
        let mut setup_buf = [0u8; MAX_PACKET_HEADER_BYTES];
        let written = header.write_to_slice(&mut setup_buf).unwrap();
        let encoded = &setup_buf[..written];

        b.iter(|| black_box(PacketHeader::read(black_box(encoded))));
    });

    group.finish();
}

fn benchmark_sequence_buffer(c: &mut Criterion) {
    let mut group = c.benchmark_group("sequence_buffer");

    group.bench_function("insert_and_find", |b| {
        let config = EndpointConfig::default();
        let mut endpoint = Endpoint::new(config, 0.0);

        b.iter(|| {
            for i in 0..100u16 {
                endpoint.send_packet(black_box(&[i as u8; 32]));
            }
            let mut count = 0usize;
            endpoint.drain_outgoing(|_, _| count += 1);
            black_box(count)
        });
    });

    group.finish();
}

fn benchmark_ack_processing(c: &mut Criterion) {
    let mut group = c.benchmark_group("ack_processing");

    group.bench_function("full_roundtrip", |b| {
        let config = EndpointConfig::default();
        let mut client = Endpoint::new(config.clone(), 0.0);
        let mut server = Endpoint::new(config.clone(), 0.0);

        // Preallocate staging buffer (outside measured section)
        let mut transfer_buf: Vec<Vec<u8>> = Vec::with_capacity(64);

        b.iter(|| {
            // Reset endpoints for clean state
            client.reset();
            server.reset();

            // Send 32 packets from client
            for i in 0..32u8 {
                client.send_packet(black_box(&[i; 64]));
            }

            // Transfer client -> server (collect then deliver to allow reset between phases)
            transfer_buf.clear();
            client.drain_outgoing(|_, data| transfer_buf.push(data.to_vec()));
            for data in &transfer_buf {
                server.receive_packet(data);
            }
            // Consume incoming
            let mut in_count = 0usize;
            server.drain_incoming(|_, _| in_count += 1);
            black_box(in_count);

            // Server sends response (triggers ACK)
            server.send_packet(b"ack");

            // Transfer server -> client
            transfer_buf.clear();
            server.drain_outgoing(|_, data| transfer_buf.push(data.to_vec()));
            for data in &transfer_buf {
                client.receive_packet(data);
            }

            // Measure ack count
            black_box(client.get_acks().len())
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    benchmark_send_receive,
    benchmark_packet_header,
    benchmark_sequence_buffer,
    benchmark_ack_processing,
);

criterion_main!(benches);
