# Session Summary: Hot Path Allocation Audit and Zero-Alloc Optimization Plan

**Date:** 2026-05-22<br>
**Duration:** ~15 interactions<br>
**Focus Area:** `src/endpoint.rs`, `src/fragment.rs` - hot path allocation violations and remediation plan<br>

## Objectives

- [x] Evaluate Glommio integration feasibility as a performance proof vehicle
- [x] Conduct full audit of hot path allocations across all source files
- [x] Produce actionable implementation plan to reach true zero-alloc steady state
- [x] Implement Steps 1, 2, 3 (PacketQueue, config fields, ReassemblyData slab)
- [x] Implement Steps 4-7 (Endpoint refactor, new API, version bump, bench + docs)

## Work Completed

### Glommio Integration Evaluation

Analyzed whether integrating the Glommio thread-per-core async runtime would be useful for proving
performance. Conclusion: **not yet, and wrong abstraction layer.**

Key findings:
- `reliable-rs` is an I/O-agnostic protocol core - no UDP socket code exists anywhere
- Current Criterion benchmarks already cover the protocol hot path (~79 ns, 7.3 GiB/s)
- Glommio measures `io_uring` socket throughput, not reliability protocol correctness
- Glommio is Linux kernel >= 5.8 only - adds platform lock-in without informing protocol design
- Prerequisite: hot path allocations must be fixed before any I/O layer benchmark is meaningful

Recommended path before any async I/O work:
1. Fix hot path allocations (this plan)
2. Add `std::net::UdpSocket` example as cross-platform baseline
3. Optionally add Tokio feature-gated example (mainstream, cross-platform)
4. Revisit Glommio only if a specific thread-per-core deployment is a documented target

### Full Allocation Audit

Audited all 7 source files. Found **10 allocation violations** across `endpoint.rs` and `fragment.rs`.
All other files (`packet.rs`, `sequence_buffer.rs`, `utils.rs`, `config.rs`, `lib.rs`) are clean.

| ID | File | Location | Violation | Status |
|----|------|----------|-----------|--------|
| A | `endpoint.rs:192` | `send_regular_packet` | `Vec::with_capacity(...)` per packet send | Pending Step 4 |
| B | `endpoint.rs:214` | `send_fragmented_packet` | `Vec::with_capacity(...)` per fragment | Pending Step 4 |
| C | `endpoint.rs:292` | `receive_regular_packet` | `payload.to_vec()` per received packet | Pending Step 4 |
| D | `endpoint.rs:411` | `take_outgoing_packets` | `.drain(..).collect()` allocates new `Vec` per call | Pending Step 5 |
| E | `endpoint.rs:415` | `take_incoming_packets` | `.drain(..).collect()` allocates new `Vec` per call | Pending Step 5 |
| F | `fragment.rs:136` | `ReassemblyData.fragments` | `Vec<Vec<u8>>` - two levels of heap, unbounded | **Fixed Step 3** |
| G | `fragment.rs:231` | `process_fragment` | `vec![Vec::new(); num_fragments]` per new reassembly | **Fixed Step 3** |
| H | `fragment.rs:256` | `process_fragment` | `fragment_data.to_vec()` per fragment received | **Fixed Step 3** |
| I | `fragment.rs:294` | `fragment_packet` | `Vec::with_capacity` + `.to_vec()` per fragment | Pending Step 4 |
| J | `fragment.rs:161` | `reassemble` | `Vec::new()` at packet completion | Pending Step 4 (`reassemble_to_vec` shim) |

Additional finding: `acks: Vec<u16>` in `Endpoint` is unbounded and grows during ACK processing.
Will be replaced with `ack_buf: Box<[u16]>` + `ack_count: usize` in Step 4.

### 7-Step Implementation Plan

Full plan with exact struct definitions, method signatures, and file-by-file changes:

#### Step 1 - New file `src/packet_queue.rs`

New `PacketQueue` struct: fixed-capacity ring buffer using `Box<[PacketSlot]>` preallocated at init.
Each `PacketSlot` holds `data: Box<[u8]>` preallocated with a fixed `slot_capacity`.

Key method: `write_slot(seq: u16, f: impl FnOnce(&mut [u8]) -> usize) -> bool` - closure-based
in-place write pattern; no intermediate Vec, no copy overhead.

Indexing: `ptr & (capacity - 1)` always, capacity enforced as power-of-two.

Two instantiations in `Endpoint`:
- `outgoing_queue`: `slot_capacity = config.max_datagram_size()` - per-datagram size
- `incoming_queue`: `slot_capacity = config.max_packet_size` - reassembled payload size

Module-level `#![allow(dead_code)]` added - removed when Step 4 wires it into `Endpoint`.

#### Step 2 - `src/config.rs` additions

New fields: `outgoing_queue_size: usize` (default 256) and `incoming_queue_size: usize` (default 256),
both power-of-two enforced in `validate()`.

New computed method: `max_datagram_size() -> usize` returning
`fragment_size + FRAGMENT_HEADER_BYTES + MAX_PACKET_HEADER_BYTES`.

#### Step 3 - `src/fragment.rs` - `ReassemblyData` slab refactor

Replaced `fragments: Vec<Vec<u8>>` with:
- `fragment_data: Box<[u8]>` - flat slab, `max_fragments * fragment_size`, preallocated once
- `fragment_lens: Box<[u16]>` - per-fragment length array, preallocated once
- `fragment_size: usize` - cached for offset arithmetic

New methods:
- `reset(seq, ack, ack_bits, num_fragments)` - resets metadata, retains slab
- `store_fragment(id, src)` - zero-alloc write into slab
- `copy_to(dest: &mut [u8]) -> usize` - zero-alloc reassembly into caller buffer (Step 4)
- `reassemble_to_vec()` - **temporary shim** (private), removed in Step 4

`FragmentReassemblyBuffer` now uses `SequenceBuffer<()>` (tracker only) + `Box<[ReassemblyData]>`
(preallocated slab pool). `ReassemblyData` no longer needs `Clone + Default`.

`fragment_packet()` changed to `pub(crate)` - test-only, not called on hot path.

`has_fragment` / `mark_fragment`: migrated from `% 32` / `/ 32` to bitwise `& 31` / `>> 5`.

`fragment_size` field removed from `FragmentReassemblyBuffer` (redundant - each slot caches its own).

#### Step 4 - `src/endpoint.rs` - Endpoint struct refactor

Three field replacements:

| Removed | Replaced with |
|---------|---------------|
| `outgoing_packets: VecDeque<(u16, Vec<u8>)>` | `outgoing_queue: PacketQueue` |
| `incoming_packets: VecDeque<(u16, Vec<u8>)>` | `incoming_queue: PacketQueue` |
| `acks: Vec<u16>` | `ack_buf: Box<[u16]>` + `ack_count: usize` |

All send/receive/ack methods rewritten to use `write_slot` closures and slab `copy_to`.
`send_fragmented_packet` inlines fragment logic directly - no longer calls `fragment_packet`.
Remove `#![allow(dead_code)]` from `packet_queue.rs` once wired up.
Remove `reassemble_to_vec` shim from `fragment.rs` once `copy_to` is used directly.

#### Step 5 - New public API, deprecate old

New zero-alloc methods:
```
pub fn drain_outgoing(&mut self, f: impl FnMut(u16, &[u8]))
pub fn drain_incoming(&mut self, f: impl FnMut(u16, &[u8]))
```

Deprecated wrappers kept for smooth migration:
```
#[deprecated(since = "0.2.0", note = "use drain_outgoing to avoid allocation")]
pub fn take_outgoing_packets(&mut self) -> Vec<(u16, Vec<u8>)>
```

#### Step 6 - Version bump and call site migration

- `Cargo.toml`: `0.1.0` -> `0.2.0`
- `tests/integration.rs`: 11 call sites migrated to `drain_*` closures
- `examples/basic.rs`, `examples/client_server.rs`, `examples/with_packet_loss.rs`: updated
- `benches/throughput.rs`: 8 call sites updated

Special case: `test_out_of_order_fragments` needs local Vec to reverse packets before delivery -
acceptable since it is a test, not hot path code.

#### Step 7 - CI validation and perf docs update

Mandatory sequence: `cargo fmt --all` -> `cargo clippy ... -D warnings` -> `cargo test --workspace`
-> `cargo bench`. Update `docs/performance_design.md` Performance Targets table with new numbers.
Regression policy: > 10% on any metric requires investigation before merging.

## Decisions Made

| Decision | Rationale | ADR |
|----------|-----------|-----|
| Do not integrate Glommio now | Wrong abstraction layer; measures I/O driver not protocol; Linux-only; hot-path allocs not fixed yet | N/A |
| `Box<[u8]>` preallocated at init for slot buffers | Single heap allocation at startup; zero alloc in steady state; avoids fixed const generics | N/A |
| `write_slot(f: impl FnOnce(&mut [u8]) -> usize)` closure pattern | Enables in-place writes into pre-allocated slot without intermediate buffer or copy | N/A |
| Two `PacketQueue` instances with different `slot_capacity` | outgoing slots need datagram size only; incoming slots need full reassembled payload size | N/A |
| Keep `fragment_packet` as `pub(crate)` | Has valuable unit tests; remove in later session after tests migrated to Endpoint-level | N/A |
| Replace `acks: Vec<u16>` with `Box<[u16]>` + count | Removes last unbounded growth in steady state; `ack_buf` sized to `sent_packets_buffer_size` | N/A |
| Deprecate `take_*` rather than remove | Breaking change mitigation; `0.2.0` semver bump signals intent | [ADR-001](../decisions/ADR-001-zero-alloc-drain-api.md) |
| `SequenceBuffer<()>` for tracker + separate `Box<[ReassemblyData]>` | Avoids `Default` requirement on `ReassemblyData`; preallocated slabs survive `SequenceBuffer::insert` overwrites | N/A |
| Module-level `#![allow(dead_code)]` on `packet_queue.rs` | Clean WIP suppression; removed atomically when Step 4 wires the module | N/A |
| `process_fragment` accepts `&mut PacketQueue` parameter | Eliminates `reassemble_to_vec` shim; writes directly into caller-supplied slot; avoids Vec allocation at reassembly completion | [ADR-002](../decisions/ADR-002-process-fragment-queue-param.md) |
| `ack_buf` full: silent drop + `log::debug!` | Bounded buffer; burst ACKs beyond `sent_packets_buffer_size` are re-acked on next packet; drop is safe, logged for diagnostics | [ADR-003](../decisions/ADR-003-bounded-ack-buf.md) |
| Fragment test `slot_capacity = 4096` | Covers all test payload sizes (max 1000 bytes) with headroom; avoids fragile size coupling | N/A |
| Borrow split in `receive_fragment`: direct disjoint field borrow | `self.fragment_reassembly` and `self.incoming_queue` are separate struct fields; Rust 2021 NLL allows concurrent mutable borrows of disjoint fields | N/A |

## Tests Added/Modified

| Test File | Change | Step | Status |
|-----------|--------|------|--------|
| `src/packet_queue.rs` | 10 new unit tests for `PacketQueue` | Step 1 | **Done** |
| `src/fragment.rs` | 2 new tests: `test_reassembly_reset_reuses_slab`, `test_copy_to` | Step 3 | **Done** |
| `src/fragment.rs` | Existing 6 tests: kept, all pass with new implementation | Step 3 | **Done** |
| `src/fragment.rs` | 5 tests updated: `process_fragment` call sites use `PacketQueue(16, 4096)` + drain | Step 4 | **Done** |
| `src/endpoint.rs` unit tests | All 7 tests migrated from `take_*` to `drain_*` | Step 6 | **Done** |
| `tests/integration.rs` | 11 call sites: `take_outgoing/incoming_packets` -> `drain_*` | Step 6 | **Done** |
| `benches/throughput.rs` | 8 call sites updated to `drain_*` | Step 6 | **Done** |

## Issues Encountered

| Issue | Resolution | Blocking |
|-------|------------|----------|
| `incoming_queue` slot size different from `outgoing_queue` | Use `PacketQueue::new(size, slot_capacity)` with `max_packet_size` for incoming vs `max_datagram_size()` for outgoing | No |
| `ReassemblyData` needs `Default` derive but `Box<[u8]>` empty slice is not meaningful | Removed `Default` derive; switched `FragmentReassemblyBuffer` to `SequenceBuffer<()>` + separate preallocated slot array | No |
| `test_out_of_order_fragments` reverses packet slice after collection | Acceptable local Vec collect in test code only; not hot path | No |
| `pub(crate)` items in new `packet_queue.rs` flagged as dead_code by clippy | Added module-level `#![allow(dead_code)]`; removed when Step 4 wires the module | No |
| `fragment_size` field in `FragmentReassemblyBuffer` was redundant | Removed - each `ReassemblyData` slot already stores its own `fragment_size` | No |

## CI Results (after Steps 1-3)

```
cargo fmt --all         -- ok
cargo clippy ... -D warnings  -- ok (0 warnings)
cargo test --workspace  -- 60/60 passed
  - 47 unit tests (lib)
  - 10 integration tests
  - 3 doc tests
```

## Next Steps

1. ~~**High:** Implement Step 1 - create `src/packet_queue.rs`~~ Done
2. ~~**High:** Implement Step 2 - add config fields and `max_datagram_size()`~~ Done
3. ~~**High:** Implement Step 3 - refactor `ReassemblyData` to flat slab~~ Done
4. ~~**High:** Implement Step 4 - refactor `Endpoint` fields and all send/receive/ack methods~~ Done
5. ~~**High:** Implement Step 5 - add `drain_outgoing` / `drain_incoming`, deprecate `take_*`~~ Done
6. ~~**Medium:** Implement Step 6 - bump to `0.2.0`, migrate all call sites~~ Done
7. ~~**Medium:** Implement Step 7 - CI sequence, update `docs/performance_design.md`~~ Done
8. **Low:** After all above: add `std::net::UdpSocket` loopback example as first real I/O baseline

## Files Changed

| Status | File | Steps |
|--------|------|-------|
| A | `src/packet_queue.rs` | Step 1 |
| M | `src/lib.rs` | Step 1, 6 |
| M | `src/config.rs` | Step 2 |
| M | `src/fragment.rs` | Step 3, 4 |
| M | `src/packet_queue.rs` - removed `#![allow(dead_code)]` | Step 4 |
| M | `src/endpoint.rs` - full struct + method refactor + drain API | Step 4, 5 |
| M | `Cargo.toml` - version bump to `0.2.0` | Step 6 |
| M | `tests/integration.rs` - migrated 11 call sites | Step 6 |
| M | `examples/basic.rs` - drain API | Step 6 |
| M | `examples/client_server.rs` - drain API | Step 6 |
| M | `examples/with_packet_loss.rs` - drain API | Step 6 |
| M | `benches/throughput.rs` - migrated 8 call sites | Step 6 |
| A | `docs/decisions/ADR-001-zero-alloc-drain-api.md` | Step 4 |
| A | `docs/decisions/ADR-002-process-fragment-queue-param.md` | Step 4 |
| A | `docs/decisions/ADR-003-bounded-ack-buf.md` | Step 4 |
| M | `docs/performance_design.md` - updated perf numbers | Step 7 |
