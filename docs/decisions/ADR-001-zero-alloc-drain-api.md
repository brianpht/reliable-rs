# ADR-001: Zero-Alloc Drain API replaces allocating take_* methods

**Date:** 2026-05-22<br>
**Status:** Accepted<br>
**Deciders:** developer<br>
**Related Tasks:** Steps 5, 6<br>
**Related ADRs:** [ADR-002](ADR-002-process-fragment-queue-param.md), [ADR-003](ADR-003-bounded-ack-buf.md)<br>
**Related Sessions:** [Session 2026-05-22](../sessions/2026-05-22-hot-path-alloc-audit.md)<br>

## Context

`Endpoint::take_outgoing_packets()` and `Endpoint::take_incoming_packets()` are the primary
API surface for callers to read queued datagrams and received payloads. Both methods call
`.drain(..).collect()` on internal `VecDeque` structures, which allocates a new `Vec<(u16,
Vec<u8>)>` on every call. This happens on the hot path (called once per tick), and each entry
in the returned Vec contains a second heap allocation for the `Vec<u8>` payload.

With the internal `PacketQueue` ring buffer introduced in Step 1 using preallocated
`Box<[u8]>` slots, the internal representation is zero-alloc. The bottleneck is the public
API: callers must receive their data somehow, and the current API forces a heap allocation to
do so.

The public API must change to expose the zero-alloc property to callers.

## Options Considered

### Option A: Closure-based drain (zero-alloc)

```rust
pub fn drain_outgoing(&mut self, f: impl FnMut(u16, &[u8]))
pub fn drain_incoming(&mut self, f: impl FnMut(u16, &[u8]))
```

Callers receive a `&[u8]` borrowed from the preallocated slot. No heap allocation occurs.
Callers who need to copy (e.g., for deferred processing) allocate explicitly and knowingly.

- **Pros:** True zero-alloc hot path; zero-copy if caller processes inline; exposes buffer
  lifetime clearly; composable with any pattern (copy, forward, ignore)
- **Cons:** Borrowing rule prevents calling `server.receive_packet(data)` inside a closure on
  `client` if both are `&mut` at the same time; test code must collect explicitly
- **Effort:** Impl: Low / Migration: Medium (11 integration sites, 8 bench sites, 3 examples) /
  Maintenance: Low

### Option B: Keep take_* allocating API

No change. Keep `VecDeque<(u16, Vec<u8>)>` internally, keep allocating `take_*` methods.

- **Pros:** Zero migration effort
- **Cons:** Violates allocation-free hot path invariant; unbounded latency variance per tick;
  every send/receive cycle allocates
- **Effort:** Impl: None / Migration: None / Maintenance: Medium (performance debt grows)

### Option C: Iterator-based API

Return a custom `DrainIter` struct implementing `Iterator<Item = (u16, &[u8])>`. Zero-alloc
but adds lifetime complexity (iterator borrows self, preventing concurrent sends).

- **Pros:** Familiar Rust idiom
- **Cons:** Borrow lifetime on iterator prevents concurrent mutable access to endpoint state;
  more complex type surface; still requires explicit lifetime annotation at call sites
- **Effort:** Impl: High / Migration: Medium / Maintenance: High

## Decision

**Chosen: Option A - Closure-based drain (zero-alloc)**

## Rationale

Closure pattern eliminates allocation at the API boundary. The `&[u8]` slice passed to the
closure is valid only for the duration of the call, which is the correct lifetime model for
a slot in a ring buffer that will be overwritten on the next cycle.

The migration cost (Option A medium vs Option B none) is a one-time expense. Option B
accumulates latency variance debt indefinitely. Option C has higher complexity for equivalent
zero-alloc property.

Deprecated `take_*` wrappers are retained to avoid breaking existing users immediately.
The `#[deprecated(since = "0.2.0", ...)]` attribute provides a clear migration signal at the
`0.2.0` semver boundary.

## Consequences

- **Positive:** Hot path is allocation-free end-to-end; latency variance from heap allocator
  removed; callers have explicit control over when/whether to copy
- **Negative:** Breaking change in calling convention (closure vs for-loop over Vec); test
  code that needs to reverse packet order must collect explicitly first
- **Neutral:** The deprecated `take_*` wrappers add ~10 lines of compatibility shim code

## Affected Components

| Component | Impact | Description |
|-----------|--------|-------------|
| `src/endpoint.rs` | High | New `drain_outgoing` / `drain_incoming` methods; deprecated `take_*` wrappers |
| `tests/integration.rs` | Medium | 11 call sites migrated |
| `benches/throughput.rs` | Medium | 8 call sites migrated |
| `examples/*.rs` | Low | 3 examples updated |
| `src/lib.rs` | Low | Doctest updated |

## Revisit When

- A second consumer of the same queue is needed (e.g., multi-reader) - closure pattern may need
  to be replaced with a snapshot/epoch mechanism
- Async runtime integration requires the queue to integrate with wakers - the closure pattern
  does not compose with async poll loops

## Migration Plan

1. Add `drain_outgoing` / `drain_incoming` to `Endpoint` (Step 5)
2. Mark `take_outgoing_packets` / `take_incoming_packets` as `#[deprecated(since = "0.2.0")]`
3. Bump `Cargo.toml` to `0.2.0` (Step 6)
4. Migrate all call sites in tests, examples, and benches (Step 6)

