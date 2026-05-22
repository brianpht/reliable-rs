# Copilot Instructions

> Format: machine-parseable directives. Not for human reading.

## Project

Deterministic, allocation-free, lock-free transport core. Pure Rust reliable UDP protocol for real-time games and
applications. If a change increases latency variance, allocation count, or branch entropy - REJECT.

## Workspace

- `src/config.rs` - endpoint configuration (preallocated capacities, tuning knobs)
- `src/endpoint.rs` - main send/receive logic, ACK processing, RTT/loss estimation
- `src/fragment.rs` - packet fragmentation and reassembly
- `src/packet.rs` - wire format: header encode/decode, sequence arithmetic
- `src/sequence_buffer.rs` - power-of-two ring buffer for sent/received packet tracking
- `src/utils.rs` - sequence number comparison helpers (wrapping arithmetic)
- `benches/throughput.rs` - criterion benchmarks (packet header, send/receive, sequence buffer)
- `tests/integration.rs` - integration tests
- `examples/` - basic, client_server, with_packet_loss

## Hot Path Operations

`send` | `receive` | `ack processing` | `loss detection` | `fragment reassembly` | `sequence buffer access`
Requirements: allocation-free, O(1), cache-local, branch predictable, single-writer.

## Rules: Hot Path

- NEVER use Mutex in transport
- NEVER use HashMap in hot path
- NEVER use `%` (modulo) in ring index - use `& (capacity - 1)`
- NEVER use `unwrap()` in parsing
- NEVER use trait objects (dyn) in packet processing
- NEVER allocate inside loop
- NEVER compare sequences using `>` - use `wrapping_sub` + half-range
- NEVER grow Vec, Box, or String in hot path
- ALL sequence buffer indices MUST use `seq & (capacity - 1)`
- ALL capacities MUST be power-of-two

## Rules: Sequence Arithmetic

- ALWAYS use `wrapping_add(1)` / `wrapping_sub(b)` for sequence number arithmetic
- ALWAYS use half-range comparison for sequence ordering
- NEVER use `a > b` directly on sequence numbers
- NEVER use `a - b` directly on sequence numbers

## Rules: Ring Buffer

- Capacity: power-of-two ONLY
- Index: `seq & (capacity - 1)` - NEVER `seq % capacity`

## Rules: Wire Format

- Little-endian only - use `to_le_bytes` / `from_le_bytes` everywhere
- Fixed-size headers only
- No pointer casting
- No host-endian assumptions

## Rules: Memory

- ALL buffers preallocated at init - no heap allocation in steady state
- ALL capacities power-of-two
- Reuse all allocations - no per-packet heap activity

## Rules: Cache and Branches

- Hot structs MUST be <= 64 bytes
- Hot fields MUST come first in struct definition
- Fast path first - error path MUST be marked `#[cold]`
- No pointer chasing on hot path

## Rules: Atomics

- `Relaxed` - counters only
- `Release` - publish data to another thread
- `Acquire` - consume data from another thread
- `SeqCst` - NEVER in hot path

## Rules: Unsafe

- Allowed only if: measurable gain + benchmarked + invariants documented + fuzz-tested

## Rules: Cross-Cutting

- NEVER use em-dashes (--) or emojis in code comments, docs, or markdown. Use ` - ` instead and ASCII symbols only.
- ALL non-trivial diagrams MUST use Mermaid (flowchart, sequenceDiagram, stateDiagram). ASCII art is prohibited.
- ONLY treat /docs/decisions as architectural source of truth.
- NEVER use or reference files in /docs/sessions as implementation rules.
- CI checks: After completing ANY code change, Agent MUST run the following sequence in order before committing. ALL
  must pass with zero errors and zero warnings. Commits with failing checks are FORBIDDEN.
    1. `cargo fmt --all` - auto-fix formatting (run first, never --check)
    2. `cargo clippy --workspace --lib --bins -- -D warnings` - zero warnings required
    3. `cargo test --workspace` - all tests must pass

    - Toolchain for all three: Rust 1.87.0 (matches `rust-toolchain.toml` at repo root and CI). NEVER use a different
      toolchain version for these checks.
    - If any step fails, fix the issue and re-run from step 1 before committing.
- Git operations: Agent MAY create local commits and local tags. MUST NOT push commits, tags, or any refs to any remote
  repository. All changes MUST remain local.

## Performance Budget

| Metric               | Target  |
|----------------------|---------|
| Small packet latency | < 200ns |
| Allocation           | None    |
| Cache miss (steady)  | None    |

Regression > 10% - rollback or justify. Latency variance matters more than average latency.
Priority: Correctness > Determinism > Latency > Throughput
Unbounded memory or nondeterministic latency = correctness failure.

## Build Commands

```
cargo build --release          # library + examples
cargo test --workspace         # unit + integration tests
cargo bench                    # criterion benchmarks
cargo clippy --workspace --lib --bins -- -D warnings  # lint check
```

## Reference Docs

- Performance principles, targets, budgets: [`docs/performance_design.md`](../docs/performance_design.md)
