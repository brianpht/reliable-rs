//! Throughput benchmarks for reliable-rs

use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
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
                server.receive_packet(&packet);
            }
            server.take_incoming_packets();
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
                server.receive_packet(&packet);
            }
            server.take_incoming_packets();
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
                server.receive_packet(&packet);
            }
            server.take_incoming_packets();
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
            header.write(black_box(&mut buffer));
        });
    });

    group.bench_function("read", |b| {
        let header = PacketHeader::new(1000, 998, 0xFFFFFFFF);
        let mut buffer = Vec::new();
        header.write(&mut buffer);

        b.iter(|| {
            PacketHeader::read(black_box(&buffer))
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
            endpoint.take_outgoing_packets();
        });
    });

    group.finish();
}

fn benchmark_ack_processing(c: &mut Criterion) {
    let mut group = c.benchmark_group("ack_processing");

    group.bench_function("full_roundtrip", |b| {
        let config = EndpointConfig::default();

        b.iter(|| {
            let mut client = Endpoint::new(config.clone(), 0.0);
            let mut server = Endpoint::new(config.clone(), 0.0);

            // Send 32 packets
            for i in 0..32 {
                client.send_packet(&[i as u8; 64]);
            }

            // Transfer to server
            for (_, data) in client.take_outgoing_packets() {
                server.receive_packet(&data);
            }

            // Server responds
            server.send_packet(b"ack");

            // Transfer back to client
            for (_, data) in server.take_outgoing_packets() {
                client.receive_packet(&data);
            }

            // Process acks
            let acks = client.get_acks().len();
            black_box(acks);
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