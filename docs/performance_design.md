# Performance Design

> **High-performance, low-latency reliable UDP protocol implementation in Rust.**

---

## Table of Contents

- [Project Overview](#project-overview)
- [Governance Model](#governance-model)
- [Deployment Assumptions](#deployment-assumptions)
- [Performance Targets](#performance-targets)
- [Core Design Principles](#core-design-principles)
  - [1. Determinism First](#1-determinism-first)
  - [2. Zero-Copy Where Possible](#2-zero-copy-where-possible)
  - [3. Allocation-Free Hot Path](#3-allocation-free-hot-path)
  - [4. Cache-Oriented Design](#4-cache-oriented-design)
  - [5. Branch Predictability](#5-branch-predictability)
  - [6. Lock-Free Model](#6-lock-free-model)
  - [7. Sequence Arithmetic](#7-sequence-arithmetic)
  - [8. Ring Buffer Discipline](#8-ring-buffer-discipline)
  - [9. Wire Format (Little-Endian)](#9-wire-format-little-endian)
  - [10. Fragmentation](#10-fragmentation)
  - [11. Unsafe Policy](#11-unsafe-policy)
  - [12. Performance Budget](#12-performance-budget)
- [Final Principle](#final-principle)

---

## Project Overview

### Target Domains

| Domain                        | Use Case                          |
|-------------------------------|-----------------------------------|
| Real-time multiplayer engines | Game state synchronization        |
| Latency-critical systems      | Distributed systems messaging     |

### Inspiration

| Source                            | Contribution                    |
|-----------------------------------|---------------------------------|
| Glenn Fiedler's reliable protocol | Core reliability concepts       |
| Aeron transport design            | Lock-free, zero-copy principles |
| Mechanical Sympathy philosophy    | Hardware-aware optimization     |

---

## Governance Model

This project defines **two layers** of performance governance:

| Layer            | Document                          | Purpose                      |
|------------------|-----------------------------------|------------------------------|
| **Architecture** | `performance_design.md`           | Defines intent and reasoning |
| **Enforcement**  | `.github/copilot-instructions.md` | Enforces non-negotiable rules|

### Conflict Resolution

```
If enforcement rules conflict with architecture
    -> Architecture must be updated first

Benchmarks are the final authority.
```

---

## Deployment Assumptions

| Assumption           | Value                                            |
|----------------------|--------------------------------------------------|
| Primary target       | x86_64                                           |
| Wire format          | **Little-endian (protocol-defined)**             |
| Cluster architecture | Same-architecture expected                       |
| Priority             | Deterministic latency > cross-endian portability |

> NOTE: Cross-endian compatibility is not guaranteed and would require a
> versioned protocol change.

---

## Performance Targets

| Metric                   | Target    | Current   | Status |
|--------------------------|-----------|-----------|--------|
| Small packet latency     | < 200 ns  | ~40 ns    | OK     |
| Header read              | < 5 ns    | ~1.4 ns   | OK     |
| Header write             | < 10 ns   | ~3.1 ns   | OK     |
| Throughput (fragmented)  | > 5 GiB/s | ~13 GiB/s | OK     |
| Full roundtrip (32 pkts) | < 10 us   | ~2.3 us   | OK     |

> Updated 2026-05-22 after Steps 4-7 zero-alloc refactor (v0.2.0). Previous baseline:
> ~79 ns small packet, 7.3 GiB/s fragmented, ~2.9 us roundtrip.
> All metrics improved: small packet -49%, fragmented throughput +78%, roundtrip -21%.

### Regression Policy

- **> 10% regression** - requires justification
- **Tail latency** matters more than average latency

---

## Core Design Principles

### 1. Determinism First

```
Correctness > Determinism > Latency > Throughput
```

> WARNING: Unbounded memory or nondeterministic latency is a correctness failure.

#### Must Be Deterministic Under

| Condition     | Required |
|---------------|----------|
| Packet loss   | yes      |
| Reordering    | yes      |
| Duplication   | yes      |
| Sequence wrap | yes      |

**No randomness in protocol logic.**

---

### 2. Zero-Copy Where Possible

| Principle                     | Rationale                |
|-------------------------------|--------------------------|
| Prefer borrowing over cloning | Avoid unnecessary copies |
| Memory copy cost              | ~0.5 ns per byte         |
| Hot path copying              | FORBIDDEN                |

---

### 3. Allocation-Free Hot Path

#### No Heap Allocation During

| Operation           | Allocation Allowed |
|---------------------|--------------------|
| `send`              | no                 |
| `receive`           | no                 |
| ack processing      | no                 |
| loss detection      | no                 |
| fragment reassembly | no                 |

- All buffers **preallocated at initialization**
- **Reuse everything**

---

### 4. Cache-Oriented Design

#### CPU Memory Latency Reference

| Level | Latency |
|-------|---------|
| L1    | ~1 ns   |
| L2    | ~3 ns   |
| L3    | ~10 ns  |
| RAM   | ~100 ns |

#### Rules

| Rule                          | Priority |
|-------------------------------|----------|
| Use contiguous memory         | Required |
| Avoid pointer chasing         | Required |
| Use power-of-two ring buffers | Required |
| Align hot data to cache lines | Required |
| Separate hot/cold structs     | Required |

---

### 5. Branch Predictability

**Mispredict penalty**: ~15-20 cycles (~5-7 ns)

| Rule                            | Priority       |
|---------------------------------|----------------|
| Fast path first                 | Required       |
| Error paths marked `#[cold]`    | Required       |
| Avoid data-dependent divergence | Required       |
| Prefer predictable branches     | In tight loops |

---

### 6. Lock-Free Model

**Default**: Single-writer principle.

#### Atomic Ordering (When Required)

| Ordering  | Use Case              |
|-----------|-----------------------|
| `Relaxed` | Counters              |
| `Release` | Publish data          |
| `Acquire` | Consume data          |
| `SeqCst`  | FORBIDDEN in hot path |

---

### 7. Sequence Arithmetic

| Rule                           | Status   |
|--------------------------------|----------|
| Use wrapping arithmetic        | Required |
| Half-range rule for comparison | Required |
| Naive `>` comparison           | FORBIDDEN|
| Non-wrapping subtraction       | FORBIDDEN|
| Test all wrap-around cases     | Required |

---

### 8. Ring Buffer Discipline

#### Capacity

- **MUST** be power-of-two

#### Indexing

```rust
// correct
index = seq & (capacity - 1)

// FORBIDDEN
index = seq % capacity
```

| Rule                             | Status   |
|----------------------------------|----------|
| Never use `%` in hot path        | Required |
| Overwrites must be deterministic | Required |

---

### 9. Wire Format (Little-Endian)

Wire format uses **Little-Endianness only**.

#### Rationale

| Benefit                             | Impact             |
|-------------------------------------|--------------------|
| No byte-swap overhead on x86_64     | Reduced latency    |
| Fewer instructions in header parse  | Better performance |
| Aeron-style deterministic design    | Consistency        |

#### Encoding Example

```rust
impl PacketHeader {
    fn write(&self, buffer: &mut [u8]) {
        buffer[0..2].copy_from_slice(&self.sequence.to_le_bytes());
        buffer[2..4].copy_from_slice(&self.ack.to_le_bytes());
        buffer[4..8].copy_from_slice(&self.ack_bits.to_le_bytes());
    }

    fn read(buffer: &[u8]) -> Option<Self> {
        if buffer.len() < 8 {
            return None;
        }

        Some(Self {
            sequence: u16::from_le_bytes([buffer[0], buffer[1]]),
            ack: u16::from_le_bytes([buffer[2], buffer[3]]),
            ack_bits: u32::from_le_bytes([
                buffer[4], buffer[5], buffer[6], buffer[7],
            ]),
        })
    }
}
```

#### Rules

| Rule                                     | Status                       |
|------------------------------------------|------------------------------|
| No pointer casting                       | Required                     |
| No host-endian assumptions               | Required                     |
| No unaligned loads without justification | Required                     |
| Fixed-size headers only                  | Required                     |
| Header read target                       | Single-digit nanoseconds     |

---

### 10. Fragmentation

#### Requirements

| Requirement                    | Status   |
|--------------------------------|----------|
| Bounded                        | Required |
| Allocation-free in hot path    | Required |
| DOS resistant                  | Required |
| Deterministic expiration       | Required |
| Trust fragment count from wire | NEVER    |

---

### 11. Unsafe Policy

#### Allowed Only If

| Condition                | Required |
|--------------------------|----------|
| Measurable gain proven   | yes      |
| Benchmarked before/after | yes      |
| Invariants documented    | yes      |
| Fuzz-tested              | yes      |

> FORBIDDEN: Unsafe without justification - reject.

---

### 12. Performance Budget

#### Small Packet Send Path

| Metric                  | Target       |
|-------------------------|--------------|
| Allocation              | None         |
| Latency                 | < 200 ns     |
| Steady-state cache miss | None         |

#### Investigation Trigger

```
p99 > p50 * 2 -> investigate
```

---

## Final Principle

| Layer        | Role               |
|--------------|--------------------|
| Architecture | Defines intent     |
| Enforcement  | Ensures invariants |
| Benchmarks   | Validates reality  |

```
Architecture defines intent.
Enforcement ensures invariants.
Benchmarks validate reality.
```
