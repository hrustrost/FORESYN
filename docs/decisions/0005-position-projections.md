# ADR 0005: Project PositionTaken state and rebuild it from canonical raw events

- Status: Accepted
- Date: 2026-08-14

## Context

`MarketCreated` produces an immutable market row. `PositionTaken` is different: every event updates aggregate pools and one side of one user's position. A row's latest update block is not enough to reverse a reorganization because that row may combine retained pre-ancestor state with an orphaned post-ancestor update.

The previous milestone queried and archived only `MarketCreated`. Its durable checkpoint proves progress for that filter only. Starting `PositionTaken` at the following block would silently omit historical positions and pools.

## Decision

The Alloy declarations reproduce the contract's existing `Outcome` enum and `PositionTaken(uint256,address,uint8,uint256,uint256,uint256,uint256)` ABI exactly. The indexer queries both known event signatures for the configured contract, merges results, and processes them by block number, transaction index, then log index. Unknown signatures are not projected. Raw identity remains `(chain_id, transaction_hash, log_index)`.

Normal ingestion treats successful raw insertion as the idempotency gate. For a newly inserted `PositionTaken`:

- `market_states.yes_pool` and `no_pool` are set to emitted `yesPool` and `noPool`;
- YES sets `yes_stake` to emitted `userOutcomeStake` and preserves `no_stake`;
- NO sets `no_stake` to emitted `userOutcomeStake` and preserves `yes_stake`;
- a new user row initializes the opposite stake to zero.

The projection deliberately uses emitted post-state. It does not add `amount` locally and therefore does not become a competing financial ledger.

### Historical coverage and explicit reindex

`indexer_contract_coverage.position_taken_from_block` records that this event family was ingested from the configured deployment block. It is a coverage marker, not an independently advancing checkpoint. A fresh empty index records the marker before indexing. If existing Foresyn state has no matching marker, normal startup returns a structured error and changes nothing.

For this prototype the explicit operator action is:

```text
cargo run --locked -p foresyn-backend --bin indexer -- --full-reindex
```

The flag starts one serialized PostgreSQL transaction, clears the configured chain's Foresyn index state, writes the deployment coverage marker, and commits before normal indexing begins at `FORESYN_DEPLOYMENT_BLOCK`. It never runs implicitly. Because `indexed_blocks` is chain-scoped, this relies on the documented one-Foresyn-contract-per-chain model; another `chain_id` is untouched.

### Mutable reorganization rebuild

Common-ancestor discovery remains an RPC-only phase outside SQL. Once an ancestor is known, the existing rollback transaction:

1. acquires the same chain advisory lock as normal block commits and validates the retained ancestor;
2. deletes indexed blocks above the ancestor, cascading orphaned raw logs and immutable market rows;
3. clears `market_states` and `market_positions` for the configured contract;
4. reads retained canonical `PositionTaken` raw logs from deployment through the ancestor in `(block_number, transaction_index, log_index)` order;
5. strictly decodes and reapplies their emitted post-state values;
6. restores the checkpoint to the ancestor;
7. commits atomically.

No RPC call occurs while that transaction is open. Canonical branch replay then uses the normal per-block transaction path.

## Source-of-truth boundary

The boundary is explicit:

- **blockchain = financial source of truth**;
- **raw canonical events = local replay archive** for the event families the indexer has explicitly covered;
- **`market_states` / `market_positions` = disposable read models**.

PostgreSQL remains query-optimized and repairable. It cannot authorize bets, determine settlement, or override contract balances and stakes.

## Crash guarantees

- Before rollback/rebuild COMMIT, the prior durable branch, projections, and checkpoint remain intact.
- After rollback/rebuild COMMIT but before branch replay, the checkpoint is the common ancestor and mutable rows exactly represent retained history through it.
- During replay, each block's raw events, projections, and checkpoint commit atomically. Any restart resumes from the last complete block and converges to the same state.

## Consequences and limitations

This approach favors a simple deterministic rebuild over inverse deltas or projection history tables. Rebuild cost grows with retained `PositionTaken` history. The command still performs one catch-up and exits, permits only one automatic recovery per invocation, and supports one configured Foresyn contract per chain. Adding resolution, claim, or other event projections will require explicit historical coverage and its own deterministic projection semantics; this ADR does not make the raw archive complete for events that were never queried.
