//! UDP loopback example - real I/O baseline using std::net::UdpSocket
//!
//! Two endpoints run in separate threads and exchange packets over a localhost
//! loopback socket pair. Use this as a cross-platform end-to-end latency baseline.
//!
//! Usage:
//!   cargo run --example udp_loopback
//!
//! Topology:
//!   client (127.0.0.1:7770) <---UDP---> server (127.0.0.1:7771)
//!
//! Protocol flow per round:
//!   1. Client calls send_packet("ping N")
//!   2. Client drains outgoing datagrams -> send_to server address
//!   3. Server recvs datagrams -> receive_packet -> drain_incoming -> send_packet(echo)
//!   4. Server drains outgoing datagrams -> send_to client address
//!   5. Client recvs datagrams -> receive_packet -> drain_incoming (measures RTT)

use reliable_rs::{Endpoint, EndpointConfig};
use std::net::UdpSocket;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const SERVER_ADDR: &str = "127.0.0.1:7770";
const CLIENT_ADDR: &str = "127.0.0.1:7771";

/// Number of ping-pong rounds to run.
const NUM_ROUNDS: u32 = 100;

/// Receive buffer size on the stack. Must be >= EndpointConfig::max_datagram_size().
/// Default config: fragment_size(1024) + fragment header(9) + packet header(9) = 1042.
/// 2048 gives ample headroom.
const RECV_BUF: usize = 2048;

/// Server thread: receive datagrams, echo each payload back, stop when signalled.
fn run_server(running: Arc<AtomicBool>) -> u32 {
    let socket = UdpSocket::bind(SERVER_ADDR).expect("server: bind failed");
    socket
        .set_read_timeout(Some(Duration::from_millis(2)))
        .expect("server: set_read_timeout failed");

    let config = EndpointConfig::default();
    let mut endpoint = Endpoint::new(config, 0.0);
    let start = Instant::now();

    // Re-used stack buffer - zero heap allocation in the recv loop.
    let mut recv_buf = [0u8; RECV_BUF];

    // Scratch storage for payloads that need echoing. Allocated once at thread
    // start; cleared each iteration. Not hot-path library code, just glue.
    let mut echo_payloads: Vec<Vec<u8>> = Vec::with_capacity(8);
    let mut echo_count: u32 = 0;

    while running.load(Ordering::Relaxed) {
        // Drain all pending datagrams from the socket.
        loop {
            match socket.recv_from(&mut recv_buf) {
                Ok((len, _src)) => {
                    endpoint.receive_packet(&recv_buf[..len]);
                }
                Err(ref e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    break;
                }
                Err(e) => {
                    eprintln!("server: recv error: {}", e);
                    break;
                }
            }
        }

        // Collect reassembled payloads - borrow ends before we call send_packet.
        endpoint.drain_incoming(|_, payload| {
            echo_payloads.push(payload.to_vec());
        });

        // Echo each payload back.
        for payload in &echo_payloads {
            endpoint.send_packet(payload);
            echo_count += 1;
        }
        echo_payloads.clear();

        // Send all outgoing datagrams to the known client address.
        endpoint.drain_outgoing(|_, data| {
            let _ = socket.send_to(data, CLIENT_ADDR);
        });

        endpoint.clear_acks();
        endpoint.update(start.elapsed().as_secs_f64());
    }

    echo_count
}

fn main() {
    println!("Reliable UDP - Loopback I/O Baseline\n");

    let running = Arc::new(AtomicBool::new(true));
    let running_server = Arc::clone(&running);

    let server_handle = std::thread::spawn(move || run_server(running_server));

    // Short delay to allow the server thread to bind before the client sends.
    std::thread::sleep(Duration::from_millis(20));

    // --- Client ---
    let socket = UdpSocket::bind(CLIENT_ADDR).expect("client: bind failed");
    // Generous read timeout: loopback RTT is typically < 1 ms. 50 ms allows for
    // scheduling jitter while still detecting lost datagrams in testing.
    socket
        .set_read_timeout(Some(Duration::from_millis(50)))
        .expect("client: set_read_timeout failed");

    let config = EndpointConfig::default();
    let mut endpoint = Endpoint::new(config, 0.0);
    let global_start = Instant::now();

    let mut recv_buf = [0u8; RECV_BUF];
    let mut rtts: Vec<Duration> = Vec::with_capacity(NUM_ROUNDS as usize);
    let mut timeouts: u32 = 0;

    for round in 0..NUM_ROUNDS {
        let time = global_start.elapsed().as_secs_f64();

        // Queue the payload and flush all outgoing datagrams immediately.
        let payload = format!("ping {}", round);
        endpoint.send_packet(payload.as_bytes());

        let t_send = Instant::now();
        endpoint.drain_outgoing(|_, data| {
            let _ = socket.send_to(data, SERVER_ADDR);
        });

        // Wait for the echoed reply.
        let mut got_reply = false;
        while !got_reply {
            match socket.recv(&mut recv_buf) {
                Ok(len) => {
                    endpoint.receive_packet(&recv_buf[..len]);
                    endpoint.drain_incoming(|_, _| {
                        got_reply = true;
                    });
                    // Server reply carries a piggy-backed ACK; any ACK-only
                    // outgoing datagram must also be flushed so the server can
                    // advance its RTT estimate.
                    endpoint.drain_outgoing(|_, data| {
                        let _ = socket.send_to(data, SERVER_ADDR);
                    });
                }
                Err(ref e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    eprintln!("client: round {} timed out", round);
                    timeouts += 1;
                    break;
                }
                Err(e) => {
                    eprintln!("client: recv error: {}", e);
                    break;
                }
            }
        }

        if got_reply {
            rtts.push(t_send.elapsed());
        }

        endpoint.clear_acks();
        endpoint.update(time);
    }

    // Signal the server thread to stop.
    running.store(false, Ordering::Relaxed);
    let echo_count = server_handle.join().expect("server thread panicked");

    // --- Statistics ---
    let completed = rtts.len();
    println!(
        "=== Results: {}/{} rounds completed, {} timeouts ===",
        completed, NUM_ROUNDS, timeouts
    );
    println!("Server echoes processed: {}", echo_count);

    if completed > 0 {
        let total: Duration = rtts.iter().sum();
        let avg = total / completed as u32;
        let min = rtts.iter().min().copied().unwrap();
        let max = rtts.iter().max().copied().unwrap();

        let avg_ns = avg.as_nanos() as f64;
        let variance: f64 = rtts
            .iter()
            .map(|d| {
                let diff = d.as_nanos() as f64 - avg_ns;
                diff * diff
            })
            .sum::<f64>()
            / completed as f64;
        let stddev_us = variance.sqrt() / 1_000.0;

        println!();
        println!("  RTT min:    {:>8.1} us", min.as_nanos() as f64 / 1_000.0);
        println!("  RTT avg:    {:>8.1} us", avg.as_nanos() as f64 / 1_000.0);
        println!("  RTT max:    {:>8.1} us", max.as_nanos() as f64 / 1_000.0);
        println!("  RTT stddev: {:>8.1} us  (jitter)", stddev_us);
        println!();
        println!("  Endpoint RTT estimate: {:.3} ms", endpoint.rtt());

        let c = endpoint.counters();
        println!("  Packets sent:     {}", c.packets_sent);
        println!("  Packets received: {}", c.packets_received);
        println!("  Packets acked:    {}", c.packets_acked);

        let (bw_sent, bw_recv, bw_acked) = endpoint.bandwidth();
        println!();
        println!("  Bandwidth sent:   {:.1} kbps", bw_sent);
        println!("  Bandwidth recv:   {:.1} kbps", bw_recv);
        println!("  Bandwidth acked:  {:.1} kbps", bw_acked);
    } else {
        eprintln!("No round trips completed - check that ports 7770/7771 are available.");
    }
}
