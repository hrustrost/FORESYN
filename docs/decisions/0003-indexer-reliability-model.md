# ADR 0003: Use confirmed polling with transactional per-block checkpoints

- Status: Accepted
- Date: 2026-08-14
- Reorganization handling: Superseded by [ADR 0004](0004-deterministic-reorg-recovery.md)
- PositionTaken extension: Superseded by [ADR 0005](0005-position-projections.md)

## Context

Foresyn needs a first chain-to-database path that is deterministic after downtime and does not confuse observed logs with authoritative settlement state. WebSocket-only ingestion can miss history, scanning from genesis wastes RPC capacity, and advancing a checkpoint separately from projections can permanently skip partially processed events after a crash.

The first contract event needed by product reads is the stable `MarketCreated(uint256,address,address,uint64,bytes32)` event. Reorganization rollback and every other event projection would materially expand this slice.

## Decision

The Foresyn indexer is a one-shot historical catch-up command built inside the existing Rust modular monolith. It uses Alloy over HTTP and SQLx/PostgreSQL.

On a fresh contract checkpoint it begins at `FORESYN_DEPLOYMENT_BLOCK`. On restart it first fetches the checkpoint block from the RPC and verifies that its hash is still canonical. After verification it begins at the block after the latest committed checkpoint. It computes a confirmation-aware safe head with checked subtraction and fetches only bounded block ranges, the configured contract address, and the exact `MarketCreated` signature topic. ADR 0004 defines automatic recovery when checkpoint or parent verification detects a reorganization.

Blocks are processed in ascending order. Before accepting each block after the first, its parent hash must equal the previously committed block hash. The original decision stopped on a mismatch; ADR 0004 supersedes that behavior with bounded common-ancestor recovery and canonical replay.

Each accepted block has one database transaction containing:

- its canonical number, hash, parent, and timestamp;
- raw matching logs keyed by `(chain_id, transaction_hash, log_index)`;
- projections for newly inserted `MarketCreated` logs;
- the contract-scoped last committed block checkpoint.

Empty blocks are committed so progress remains durable even when a range has no matching logs. Identical replay is harmless. Conflicting reuse of a block, event, or market identity fails explicitly. A malformed matching log aborts before the block transaction begins, leaving its checkpoint and projections unchanged.

The raw event table is an archive of the `MarketCreated` logs selected by this milestone's RPC filter, not a complete archive of all Foresyn contract events. Adding a later projection for an event such as `PositionTaken` therefore requires a historical backfill from `FORESYN_DEPLOYMENT_BLOCK` (or another explicitly selected starting block), with progress tracked independently from the existing checkpoint. The alternative is a later redesign of raw ingestion and checkpointing so all required event families are archived. The current checkpoint alone cannot establish that a newly added event family has been backfilled.

Market IDs use PostgreSQL `NUMERIC(78,0)`, which exactly represents the complete Solidity `uint256` range. Deadlines use `NUMERIC(20,0)` to preserve the complete `uint64` range. Fixed-width EVM values use length-constrained `BYTEA`.

## Consequences

Benefits:

- downtime recovery uses the same deterministic historical path as first startup;
- block, raw event, projection, and checkpoint state cannot be partially committed;
- database uniqueness is the final idempotency guard;
- RPC filters and bounded ranges limit unnecessary transfer and provider load;
- stored `MarketCreated` raw logs remain available for audit and rebuilding that projection.

Costs and limitations:

- reads lag the chain by the configured confirmation count;
- the command exits after catch-up rather than continuously following the head;
- a malformed matching log requires operator intervention;
- this milestone decoded only `MarketCreated`; ADR 0005 adds `PositionTaken` through an explicit full reindex;
- any further event projection still requires historical coverage or redesigned raw ingestion/checkpointing;
- retry/backoff and WebSockets remain future work; common-ancestor recovery and rollback/replay are specified by ADR 0004.

## Alternatives considered

**WebSocket subscriptions as the primary source.** Rejected because subscriptions do not supply a durable downtime recovery path.

**Checkpoint each fetched batch independently of projections.** Rejected because a crash could mark events processed before their raw rows or projections commit.

**Store Solidity integers in signed `BIGINT`.** Rejected because `uint64` and `uint256` both have valid values outside PostgreSQL's signed 64-bit range.

**Implement automatic reorganization rollback now.** Deferred so this slice can detect and stop safely without adding partially tested destructive recovery behavior.
