# ADR-003: Bounded ack_buf with silent drop and log::debug on overflow

**Date:** 2026-05-22<br>
**Status:** Accepted<br>
**Deciders:** developer<br>
**Related Tasks:** Step 4<br>
**Related ADRs:** [ADR-001](ADR-001-zero-alloc-drain-api.md)<br>
**Related Sessions:** [Session 2026-05-22](../sessions/2026-05-22-hot-path-alloc-audit.md)<br>

## Context

`Endpoint` previously tracked acknowledged sequence numbers in `acks: Vec<u16>`. This Vec
grows unboundedly during ACK processing: each call to `process_acks` iterates 32 bits of
`ack_bits` and pushes any newly-acked sequence onto the Vec. Between `clear_acks()` calls,
the Vec can grow up to 32 entries per packet received, with no upper bound.

This violates the allocation-free hot path invariant. The Vec must be replaced with a
fixed-capacity structure preallocated at endpoint construction.

The key design question is what to do when the fixed buffer is full and a new ACK arrives.

## Options Considered

### Option A: Bounded Box<[u16]> + ack_count; drop + log::debug on overflow

```rust
ack_buf: Box<[u16]>,   // sized to sent_packets_buffer_size
ack_count: usize,
```

When `ack_count >= ack_buf.len()` on a new ACK:
- Emit `log::debug!("ack_buf full, dropping ack {}", ack_sequence)`
- Do not write; increment `packets_acked` counter regardless

- **Pros:** Zero-alloc; deterministic memory; drop is safe because ACKs are advisory (the
  application re-checks via `get_acks()` at its own pace); already tracked in `packets_acked`
  counter; debug log provides observability without production overhead
- **Cons:** ACK notifications can be silently dropped if application calls `clear_acks()` too
  infrequently; requires documentation of the contract
- **Effort:** Impl: Low / Migration: None / Maintenance: Low

### Option B: Bounded Box<[u16]>; overwrite oldest on overflow (ring behavior)

Track `head` and `tail` pointers; new ACKs overwrite oldest entry when full.

- **Pros:** No drops; all ACKs eventually visible
- **Cons:** Non-trivial ring cursor state; application sees ACKs in different order; oldest
  ACKs disappear silently which can confuse RTT and loss logic that depends on ACK ordering;
  more complex
- **Effort:** Impl: Medium / Migration: None / Maintenance: Medium

### Option C: Keep Vec<u16>; no change

- **Pros:** No change required
- **Cons:** Unbounded growth; never cleared between frames produces unbounded Vec; violates
  allocation-free invariant
- **Effort:** Impl: None / Migration: None / Maintenance: High (performance debt)

## Decision

**Chosen: Option A - Bounded `Box<[u16]>` + `ack_count`; silent drop + `log::debug!`**

## Rationale

ACK notifications in this protocol are advisory: the application observes which sequences
were acknowledged but the reliability mechanism (loss detection, RTT) uses the internal
`SentPacketData.acked` flag directly - not the `ack_buf` slice. A dropped entry in `ack_buf`
does not cause packet loss or incorrect RTT; it only means the application's `get_acks()` call
misses that one sequence number in the current batch.

The buffer is sized to `sent_packets_buffer_size` (default 256). In steady-state operation,
`process_acks` sees at most 32 newly-acked sequences per received packet. A single call to
`clear_acks()` per tick is sufficient to drain the buffer well before it fills.

The `log::debug!` call is on the error path (`#[cold]` semantically), not the success path,
so it has zero cost in normal operation.

Option B's overwrite semantics break the invariant that `get_acks()` returns sequences in
receipt order, complicating application-level RTT measurement.

## Consequences

- **Positive:** Zero heap activity during ACK processing; deterministic memory usage;
  `ack_buf` capacity is auditable at construction time
- **Negative:** If `clear_acks()` is not called frequently, ACK notifications are dropped;
  this is documented as a contract requirement for callers
- **Neutral:** `get_acks()` still returns `&[u16]`; caller pattern is unchanged

## Affected Components

| Component | Impact | Description |
|-----------|--------|-------------|
| `src/endpoint.rs` | High | `acks: Vec<u16>` replaced by `ack_buf: Box<[u16]>` + `ack_count: usize` |
| Public API | None | `get_acks() -> &[u16]` return type unchanged |

## Revisit When

- Application needs guaranteed no-drop ACK delivery - consider a separate lock-free SPSC
  ring or callback-per-ack pattern instead of the polling `get_acks()` model
- `sent_packets_buffer_size` is reduced below 32 - `ack_buf` would fill within a single
  `process_acks` call; validate that `sent_packets_buffer_size >= 32` in `EndpointConfig::validate`

## Migration Plan

1. Replace `acks: Vec<u16>` with `ack_buf: Box<[u16]>` + `ack_count: usize` in `Endpoint` (Step 4)
2. Update `Endpoint::new` to preallocate `ack_buf` to `sent_packets_buffer_size` (Step 4)
3. Rewrite `process_acks` to use bounded write with `log::debug!` on overflow (Step 4)
4. Update `get_acks`, `clear_acks`, `reset` (Step 4)

