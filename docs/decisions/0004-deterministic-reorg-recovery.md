# ADR 0004: Recover deterministically from EVM reorganizations

- Status: Accepted
- Date: 2026-08-14

## Context

Checkpoint canonicality and parent-hash checks detect that indexed PostgreSQL state belongs to an orphaned branch, but detection alone leaves the indexer unable to resume. Recovery must remove only the divergent suffix, preserve the last shared block, and reuse the existing confirmed historical ingestion path.

## Decision

Recovery follows one sequence:

`detect -> find common ancestor -> atomic rollback -> checkpoint rewind -> canonical replay`

Both a startup checkpoint-hash mismatch and a parent mismatch while processing a newer block enter this sequence. The command permits one automatic recovery attempt. A second reorganization during replay returns an explicit recovery-limit error rather than looping.

### Common ancestor

Starting at the current stored checkpoint, the indexer walks backward through block numbers to `FORESYN_DEPLOYMENT_BLOCK`, inclusive. At each height it reads the stored `indexed_blocks` hash and fetches the canonical RPC block, validates the returned block number, and compares hashes. The first match is the common ancestor. It never scans genesis.

A missing stored block, missing RPC block, invalid RPC block number, or no match at or after deployment is an explicit error. No destructive database operation begins unless discovery succeeds.

RPC discovery deliberately happens outside the rollback transaction. Holding database locks across a potentially slow or unavailable external RPC would increase contention and transaction age without improving atomicity. The rollback transaction revalidates that the chosen ancestor and hash still exist locally before deleting anything.

### Atomic rollback and checkpoint rewind

One PostgreSQL transaction:

1. acquires a transaction-scoped advisory lock for the affected chain, shared with normal block commits, then locks checkpoint rows and the ancestor block;
2. verifies the ancestor's stored hash;
3. counts orphaned raw events and `MarketCreated` projections for observability;
4. deletes `indexed_blocks` for the affected `chain_id` strictly above the ancestor;
5. relies on existing `ON DELETE CASCADE` foreign keys to delete orphaned `blockchain_events`, `markets`, and checkpoints referencing deleted blocks;
6. inserts or updates the configured contract checkpoint to the ancestor number and hash;
7. commits.

The ancestor block, its raw events, and its market projection remain untouched. No explicit event or market delete duplicates the cascade behavior.

The block table is chain-scoped while checkpoints and projections are contract-scoped. Consequently, deleting a chain suffix also removes above-ancestor data and checkpoints for any other indexed contract on the same chain. This milestone supports one configured Foresyn contract per chain. Supporting multiple contracts on one chain requires coordinated rewind/replay or a different schema. Other `chain_id` values are isolated by every destructive predicate.

### Canonical replay

After rollback commits, the indexer reruns the normal catch-up path. The rewound checkpoint makes it start at `common_ancestor + 1`; it fetches canonical `MarketCreated` logs with `eth_getLogs`, processes blocks in ascending order, uses the existing per-block atomic commit, and stops at the confirmation-aware safe head. No separate replay writer exists.

Deleting orphaned creation blocks and replaying canonical logs is sufficient for the current immutable `MarketCreated` projection. Future mutable projections such as pools, volume, positions, resolution, and claims must be rebuilt deterministically from canonical events.

## Crash guarantees

- **Before rollback commit:** PostgreSQL rolls the transaction back, leaving the old branch and checkpoint fully intact. A later run detects the reorganization again.
- **After rollback commit but before replay:** the database durably contains the ancestor and checkpoint only. A later run resumes at `ancestor + 1`.
- **During canonical replay:** every completed block and checkpoint is durable together. A later run verifies the latest committed replay block and resumes at the following block.

## Consequences

Reorganizations within the indexed deployment window recover without operator restart, while missing history and unsupported deeper reorganizations fail without destructive mutation. The recovery remains deterministic and bounded, but it does not search before deployment, continuously poll, or coordinate multiple contracts sharing one chain-level block history.
