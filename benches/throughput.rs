//! Throughput benchmarks for reliable-rs

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use std::hint::black_box;
use reliable_rs::{Endpoint, EndpointConfig};

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
            for (_, packet) in client.take_outgoing_packets() {
                server.receive_packet(black_box(&packet));
            }
            black_box(server.take_incoming_packets().len())
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
            for (_, packet) in client.take_outgoing_packets() {
                server.receive_packet(black_box(&packet));
            }
            black_box(server.take_incoming_packets().len())
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
            for (_, packet) in client.take_outgoing_packets() {
                server.receive_packet(black_box(&packet));
            }
            black_box(server.take_incoming_packets().len())
        });
    });

    group.finish();
}

fn benchmark_packet_header(c: &mut Criterion) {
    use reliable_rs::PacketHeader;

    let mut group = c.benchmark_group("packet_header");

    group.bench_function("write", |b| {
        let header = PacketHeader::new(1000, 998, 0xFFFFFFFF);
        let mut buffer = Vec::with_capacity(16);

        b.iter(|| {
            buffer.clear();
            let written = black_box(&header).write(&mut buffer);
            black_box(written)
        });
    });

    group.bench_function("read", |b| {
        let header = PacketHeader::new(1000, 998, 0xFFFFFFFF);
        let mut buffer = Vec::new();
        header.write(&mut buffer);

        b.iter(|| {
            black_box(PacketHeader::read(black_box(&buffer)))
        });
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
            // Consume iterator and count - prevents DCE while measuring actual work
            let count = endpoint.take_outgoing_packets().len();
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

        // Preallocate transfer buffer
        let mut transfer_buf: Vec<Vec<u8>> = Vec::with_capacity(64);

        b.iter(|| {
            // Reset endpoints for clean state
            client.reset();
            server.reset();

            // Send 32 packets from client
            for i in 0..32u8 {
                client.send_packet(black_box(&[i; 64]));
            }

            // Transfer client -> server
            transfer_buf.clear();
            for (_, data) in client.take_outgoing_packets() {
                transfer_buf.push(data);
            }
            for data in &transfer_buf {
                server.receive_packet(data);
            }
            // Consume incoming
            let _ = server.take_incoming_packets().len();

            // Server sends response (triggers ACK)
            server.send_packet(b"ack");

            // Transfer server -> client
            transfer_buf.clear();
            for (_, data) in server.take_outgoing_packets() {
                transfer_buf.push(data);
            }
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