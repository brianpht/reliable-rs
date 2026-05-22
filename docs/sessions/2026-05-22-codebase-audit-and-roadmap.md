# Session Summary: Codebase Audit and Milestone Roadmap

**Date:** 2026-05-22<br>
**Duration:** ~1 session<br>
**Focus Area:** Full codebase scan - correctness gaps, dead-code artifacts, missing features, milestone planning<br>

## Objectives

- [x] Scan and analyse all source modules
- [x] Identify correctness gaps and rule violations
- [x] Document issues found with severity
- [x] Define next steps and milestones

## Work Completed

### Modules Reviewed

All source files read in full:

| File | Lines | Notes |
|------|-------|-------|
| `src/lib.rs` | 141 | Public API surface, Error types |
| `src/config.rs` | 266 | EndpointConfig, validate() |
| `src/endpoint.rs` | 854 | Main send/receive/ACK/stats logic |
| `src/packet.rs` | 397 | Wire format encode/decode |
| `src/fragment.rs` | 628 | Fragmentation and reassembly |
| `src/sequence_buffer.rs` | 341 | Power-of-two ring buffer |
| `src/packet_queue.rs` | 312 | Preallocated outgoing/incoming queue |
| `src/utils.rs` | 148 | Sequence arithmetic, EMA |
| `benches/throughput.rs` | 170 | 4 Criterion benchmark groups |
| `tests/integration.rs` | 273 | 13 integration tests |
| `docs/decisions/ADR-001..003` | - | All accepted, all implemented |
| `docs/performance_design.md` | 350 | Performance targets and governance |

### Performance Baselines (from `docs/performance_design.md`)

All targets met as of v0.2.0 after zero-alloc refactor:

| Metric | Target | Measured | Status |
|--------|--------|----------|--------|
| Small packet latency | < 200 ns | ~40 ns | OK |
| Header read | < 5 ns | ~1.4 ns | OK |
| Header write | < 10 ns | ~3.1 ns | OK |
| Throughput (fragmented) | > 5 GiB/s | ~13 GiB/s | OK |
| Full round-trip (32 pkts) | < 10 us | ~2.3 us | OK |

## Issues Encountered

### Severity: Medium

| Issue | Location | Resolution | Blocking |
|-------|----------|------------|----------|
| `ack_buffer_size` config field never used - `ack_buf` in `Endpoint::new` is sized to `sent_packets_buffer_size`, not `ack_buffer_size`; field doc is wrong | `config.rs`, `endpoint.rs:124` | Remove field or rename and wire it correctly; requires ADR decision | No |
| `Endpoint::new()` does not call `config.validate()` - invalid power-of-two sizes and out-of-range smoothing factors are accepted silently | `endpoint.rs::new()` | Add `config.validate()` call (panic or propagate `Result`) at construction time | No |
| Missing `sent_packets_buffer_size >= 32` guard - ADR-003 "Revisit When" item; buffer smaller than 32 causes `ack_buf` to overflow in a single `process_acks` call | `config.rs::validate()` | Add check to `EndpointConfig::validate` | No |

### Severity: Low

| Issue | Location | Resolution | Blocking |
|-------|----------|------------|----------|
| Stale `#[allow(dead_code)]` on `write_to_slice()` methods that are actively used in `endpoint.rs` hot path | `packet.rs:77`, `fragment.rs:70,87` | Remove the `#[allow(dead_code)]` attributes | No |
| `packet_header/write` bench calls Vec-based `write(&mut Vec)`, not the hot-path `write_to_slice()` - benchmark does not reflect production behavior | `benches/throughput.rs:75` | Replace with `write_to_slice` bench using a fixed stack buffer | No |
| `update_bandwidth()` performs O(N) full ring-buffer scan 3x per `update()` tick (N=256 default = 768 iterations/tick); no documented decision exists for this cost | `endpoint.rs::update_bandwidth()` | Milestone 4: replace with incremental delta tracking | No |

### Severity: Structural (no immediate bug, blocks future work)

| Issue | Location | Resolution | Blocking |
|-------|----------|------------|----------|
| No fuzz harness - `performance_design.md` §11 requires fuzz testing before any `unsafe` addition | - | Milestone 1: add `cargo-fuzz` harness for `receive_packet`, `PacketHeader::read`, `FragmentHeader::read` | Blocks unsafe usage |
| No `no_std` support - `std` feature flag exists in `Cargo.toml` but code uses `String`, `thiserror`, `std::iter`; library is positioned for real-time/embedded | `config.rs`, `lib.rs`, `utils.rs` | Milestone 2: gate std-only code, replace `thiserror` with `core::fmt` | No |

## Decisions Made

| Decision | Rationale | ADR |
|----------|-----------|-----|
| `ack_buffer_size` disposition deferred pending explicit decision | Two options: remove (breaking, clean) vs rename and wire correctly; needs explicit ADR | Needs ADR |
| Retransmission model deferred | Three options (app-driven, auto in `update()`, separate `ReliableChannel`); needs ADR before any implementation | Needs ADR |

## Next Steps

1. ~~**High:** Fix `Endpoint::new()` - call `config.validate()` at construction; add `sent_packets_buffer_size >= 32` check (Milestone 0)~~ Done
2. ~~**High:** Resolve `ack_buffer_size` field - write ADR-004 or inline decision, then remove or wire the field (Milestone 0)~~ Done - see [ADR-004](../decisions/ADR-004-ack-buffer-size-wired.md)
3. ~~**Medium:** Remove stale `#[allow(dead_code)]` on `write_to_slice()`; fix benchmark to call the right method (Milestone 0)~~ Done
4. **Medium:** Add `cargo-fuzz` harness for wire-format parsing (Milestone 1)
5. **Medium:** Add `proptest` for sequence arithmetic and fragment reassembly edge cases (Milestone 1)
6. **Low:** `no_std` support: gate `String`/`thiserror`/`log` on `feature = "std"` (Milestone 2)
7. **Low:** Design retransmission API via ADR, then implement `drain_unacked` (Milestone 3)
8. **Low:** Replace O(N) bandwidth scan with incremental tracking (Milestone 4)

<!-- Mark completed steps with strikethrough: ~~**High:** description~~ Done -->

## Files Changed

| Status | File |
|--------|------|
| A | `docs/sessions/2026-05-22-codebase-audit-and-roadmap.md` |
| M | `src/config.rs` |
| M | `src/endpoint.rs` |
| M | `src/packet.rs` |
| M | `src/fragment.rs` |
| M | `tests/integration.rs` |
| M | `benches/throughput.rs` |
| A | `docs/decisions/ADR-004-ack-buffer-size-wired.md` |

## Milestone Summary

| Milestone | Version | Scope | Status |
|-----------|---------|-------|--------|
| 0 - Correctness hardening | v0.2.1 | `config.validate()` in `Endpoint::new`; `ack_buffer_size` fix; stale `#[allow(dead_code)]`; benchmark method fix | Planned |
| 1 - Fuzz and property tests | v0.2.2 | `cargo-fuzz` harness; `proptest` for sequence arithmetic and fragment reassembly | Planned |
| 2 - `no_std` support | v0.3.0 | Gate `String`/`thiserror`/`log` on `std` feature; CI no_std target | Planned |
| 3 - Retransmission | v0.4.0 | ADR-004; `retransmit_timeout_ms`; `drain_unacked` zero-alloc closure API | Planned |
| 4 - Stats hardening | v0.4.x | O(1) incremental bandwidth tracking; p99 RTT jitter (optional `stats` feature) | Planned |

