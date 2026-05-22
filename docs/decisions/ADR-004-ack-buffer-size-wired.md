# ADR-004: Wire ack_buffer_size to ack_buf allocation and enforce minimum 32

**Date:** 2026-05-22<br>
**Status:** Accepted<br>
**Deciders:** developer<br>
**Related Tasks:** Milestone 0 - Step 2<br>
**Related ADRs:** [ADR-003](ADR-003-bounded-ack-buf.md)<br>
**Related Sessions:** [Session 2026-05-22](../sessions/2026-05-22-codebase-audit-and-roadmap.md)<br>

## Context

ADR-003 introduced `ack_buf: Box<[u16]>` as a bounded, preallocated ACK notification
buffer. It was sized to `config.sent_packets_buffer_size` in `Endpoint::new`. However,
`EndpointConfig` already had a dedicated `ack_buffer_size` field (default 256) whose doc
claimed it was the "ACK sequence ring buffer capacity" - but that field was never read.

This created two bugs:
1. `ack_buffer_size` had no observable effect; callers who set it expecting a smaller or
   larger ACK notification window were silently ignored.
2. `ack_buf` was coupled to `sent_packets_buffer_size`, a semantically unrelated field
   that controls the loss-detection window.

Additionally, `Endpoint::new` accepted any `EndpointConfig` without calling
`EndpointConfig::validate()`, allowing invalid configs (non-power-of-two buffer sizes,
out-of-range smoothing factors, `fragment_above > max_packet_size`) to produce silent
misbehavior.

ADR-003 also listed as a "Revisit When" item: validate that `sent_packets_buffer_size >= 32`
to prevent `ack_buf` overflow within a single `process_acks` call (which processes up to
32 bits). This was never implemented.

## Options Considered

### Option A: Remove `ack_buffer_size` field

Remove the field entirely. Size `ack_buf` to `sent_packets_buffer_size` as ADR-003 intended.

- **Pros:** Simpler config surface; no dead field; matches ADR-003 documented intent
- **Cons:** Breaking API change; callers who happen to set `ack_buffer_size` get a compile
  error; `ack_buf` remains coupled to a semantically unrelated field
- **Effort:** Impl: Low / Migration: Medium / Maintenance: Low

### Option B: Wire `ack_buffer_size` to `ack_buf` allocation

Keep the field. Change `Endpoint::new` to use `config.ack_buffer_size` to allocate
`ack_buf`. Add `ack_buffer_size > 0` and `>= 32` validation. Fix the field doc.

- **Pros:** No breaking change; field now does what callers expect; independent tuning of
  ACK notification window vs loss-detection window; semantically correct
- **Cons:** `ack_buffer_size` does not need to be a power-of-two (it is a linear array, not
  a ring buffer); old doc said "power-of-two" which must be corrected
- **Effort:** Impl: Low / Migration: None / Maintenance: Low

## Decision

**Chosen: Option B - Wire `ack_buffer_size` to `ack_buf` allocation**

## Rationale

Option B preserves backward compatibility: existing callers who set `ack_buffer_size` now
get the expected behavior. The field was always the right abstraction; only the wiring was
missing.

Option A removes a useful tuning knob. Decoupling ACK notification capacity from the
loss-detection window (`sent_packets_buffer_size`) is correct: a high-frequency game may
want a large loss window but a small ACK notification batch, or vice versa.

The doc correction (removing the erroneous "power-of-two" and "ring buffer" claims) is low
cost and improves accuracy.

Pairing Option B with:
- Adding `config.validate()` in `Endpoint::new` (panics on invalid config at construction)
- Adding `sent_packets_buffer_size >= 32` to `validate()` (closes the ADR-003 revisit item)
- Adding `ack_buffer_size >= 32` to `validate()` (minimum for one full ACK batch)

ensures the invariants are enforced with a clear, early error message.

## Consequences

- **Positive:** `ack_buffer_size` now controls what its name says; config invariants
  enforced at construction; `sent_packets_buffer_size < 32` and `ack_buffer_size < 32`
  are detected immediately at `Endpoint::new`
- **Negative:** Tests that set `max_packet_size` or `fragment_above` without updating
  related fields now panic at `Endpoint::new` rather than silently misbehaving; three test
  sites required a one-line `max_packet_size` fix
- **Neutral:** Default values (all 256) are unchanged; no behavioral change for callers
  using `EndpointConfig::default()`

## Affected Components

| Component | Impact | Description |
|-----------|--------|-------------|
| `src/config.rs` | Medium | Fixed `ack_buffer_size` doc; added `>= 32` checks for `sent_packets_buffer_size` and `ack_buffer_size`; table annotation updated |
| `src/endpoint.rs` | Medium | Added `config.validate()` panic guard in `new()`; changed `ack_buf` allocation from `sent_packets_buffer_size` to `ack_buffer_size` |
| `src/endpoint.rs` (tests) | Low | Two test sites: added `max_packet_size = max_fragments * fragment_size` for consistency |
| `tests/integration.rs` | Low | Two test sites: added matching `max_packet_size` and `fragment_above` adjustments |

## Revisit When

- `ack_buffer_size` is set below 32 legitimately (e.g., an embedded target with very
  tight memory calling `clear_acks` multiple times per tick) - lower the minimum or make
  it a warning instead of an error
- `Endpoint::new` signature is changed to return `Result<Self, Error>` - replace the
  `unwrap_or_else(panic)` with `?` propagation

