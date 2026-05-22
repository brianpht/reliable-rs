# ADR-002: process_fragment accepts &mut PacketQueue to eliminate Vec shim

**Date:** 2026-05-22<br>
**Status:** Accepted<br>
**Deciders:** developer<br>
**Related Tasks:** Step 4<br>
**Related ADRs:** [ADR-001](ADR-001-zero-alloc-drain-api.md)<br>
**Related Sessions:** [Session 2026-05-22](../sessions/2026-05-22-hot-path-alloc-audit.md)<br>

## Context

`FragmentReassemblyBuffer::process_fragment` previously returned `Option<Vec<u8>>` on
reassembly completion. This allocation occurred on the fragment receive hot path: when the
last fragment of a logical packet arrived, a new `Vec<u8>` was heap-allocated to return the
reassembled payload.

Step 3 introduced `ReassemblyData::copy_to(dest: &mut [u8]) -> usize` for zero-alloc
reassembly into a caller-supplied buffer, plus a `reassemble_to_vec()` shim to maintain
backward compatibility with the old return type. The shim is a temporary bridge that still
allocates. It must be removed.

The question is: what is the cleanest way to deliver the reassembled payload to
`incoming_queue: PacketQueue` inside `Endpoint::receive_fragment` without an intermediate Vec?

## Options Considered

### Option A: process_fragment accepts &mut PacketQueue parameter

```rust
pub fn process_fragment(
    &mut self,
    header: &FragmentHeader,
    fragment_data: &[u8],
    ack: u16,
    ack_bits: u32,
    incoming_queue: &mut PacketQueue,
) -> Option<usize>
```

When complete, calls `incoming_queue.write_slot(seq, |buf| slot.copy_to(buf))` internally.
Returns `Some(payload_bytes)` on completion, `None` otherwise.

- **Pros:** Zero-alloc; single call site; natural ownership - the function that completes
  reassembly also delivers the result; `copy_to` called exactly once
- **Cons:** `fragment.rs` gains a dependency on `packet_queue.rs` (same crate, acceptable);
  function signature grows by one parameter; fragment-level unit tests must supply a
  `PacketQueue` instance
- **Effort:** Impl: Low / Migration: Low (fragment tests, receive_fragment call site) /
  Maintenance: Low

### Option B: Return Option<&ReassemblyData> for caller to copy_to

```rust
pub fn process_fragment(...) -> Option<u16>  // returns sequence if complete
pub fn get_slot(&self, seq: u16) -> &ReassemblyData  // caller calls copy_to separately
```

Two-step API: process returns sequence, caller calls copy_to themselves.

- **Pros:** No cross-module dependency; fragment.rs stays self-contained
- **Cons:** Two borrow regions in `receive_fragment` - after calling `process_fragment` on
  `self.fragment_reassembly`, a second borrow of `self.fragment_reassembly` is needed to get
  the slot; Rust borrow checker may require `unsafe` or additional indirection; awkward API
- **Effort:** Impl: Medium / Migration: Medium / Maintenance: High

### Option C: Keep reassemble_to_vec shim permanently

No change to the return type. Keep `Option<Vec<u8>>` forever.

- **Pros:** No migration, no API change
- **Cons:** One heap allocation per reassembled packet; violates allocation-free hot path
  invariant; contradicts the entire purpose of Steps 3-4
- **Effort:** Impl: None / Migration: None / Maintenance: High (performance debt)

## Decision

**Chosen: Option A - process_fragment accepts &mut PacketQueue parameter**

## Rationale

Option A eliminates the vec allocation at the exact source (fragment completion). The
`fragment.rs` -> `packet_queue.rs` dependency is within the same crate and is semantically
correct: fragment reassembly delivers its result into the incoming packet queue.

Option B requires a two-phase pattern that is more complex and harder to reason about,
especially when the reassembly slot may be reset between the two calls. Option C is rejected
outright as it preserves the allocation violation.

The Rust borrow checker allows `self.fragment_reassembly.process_fragment(..., &mut
self.incoming_queue)` in `receive_fragment` because `fragment_reassembly` and
`incoming_queue` are distinct fields of `Endpoint`. Rust 2021's place-based borrow checking
handles disjoint field borrows correctly.

## Consequences

- **Positive:** Reassembly completion is zero-alloc end-to-end; `reassemble_to_vec` removed;
  `#[allow(dead_code)]` on `copy_to` removed (it is now in active use)
- **Negative:** `fragment.rs` imports `crate::packet_queue::PacketQueue`; fragment unit tests
  must construct a `PacketQueue(16, 4096)` as a test helper
- **Neutral:** `Option<usize>` return type (payload bytes) replaces `Option<Vec<u8>>`; same
  Option semantics, callers check `.is_some()` identically

## Affected Components

| Component | Impact | Description |
|-----------|--------|-------------|
| `src/fragment.rs` | High | `process_fragment` signature change; import `PacketQueue`; remove `reassemble_to_vec`; 5 tests updated |
| `src/endpoint.rs` | Medium | `receive_fragment` uses `Option<usize>` for byte tracking; disjoint field borrow |
| `src/packet_queue.rs` | None | No changes; used as parameter type only |

## Revisit When

- `FragmentReassemblyBuffer` is extracted into a standalone crate - dependency on
  `PacketQueue` would become a circular or cross-crate issue; resolve by introducing a
  `WriteSlot` trait abstraction at that point
- Multiple incoming targets per endpoint are needed (fan-out) - current single `&mut
  PacketQueue` parameter would need to become a slice or callback

## Migration Plan

1. Add `use crate::packet_queue::PacketQueue` import to `fragment.rs` (Step 4)
2. Change `process_fragment` signature and body; remove `reassemble_to_vec` (Step 4)
3. Update 5 fragment unit tests to use `PacketQueue::new(16, 4096)` (Step 4)
4. Update `receive_fragment` in `endpoint.rs` to pass `&mut self.incoming_queue` (Step 4)

