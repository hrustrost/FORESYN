# ADR 0001: Keep financial truth on-chain and query projections off-chain

- Status: Accepted
- Date: 2026-08-14

## Context

Foresyn needs publicly verifiable settlement and practical product queries. Storing everything on-chain would make descriptive updates expensive and couple presentation to a permanent schema. Storing settlement in PostgreSQL would require users to trust the backend and would create two competing financial ledgers.

## Decision

The EVM contract is authoritative for:

- market lifecycle and deadline;
- accepted stake and per-address positions;
- aggregate YES and NO pools;
- authorized resolution or cancellation;
- claim eligibility and paid claims.

PostgreSQL stores:

- descriptive market metadata that does not affect settlement;
- canonical block and raw event projections;
- replayable, query-optimized market, activity, and position views;
- operational indexing state.

The frontend sends financial transactions directly through the user's wallet. The backend exposes reads from PostgreSQL and may prepare unsigned transaction data later, but it does not custody keys or claim to finalize financial state.

Every projected financial row must be traceable to a canonical chain event. Reconciliation always treats the chain as correct. A reorganization removes orphaned block/event records and causes affected projections to be rebuilt.

## Consequences

Benefits:

- settlement remains independently verifiable and usable without trusting the API;
- off-chain descriptions and read models can evolve without contract migrations;
- uniqueness constraints and transactions make event ingestion restartable and idempotent;
- the boundary is straightforward to explain and test.

Costs:

- reads are eventually consistent by at least the configured confirmation depth;
- reorganization recovery and projection replay are mandatory indexer responsibilities;
- metadata availability is weaker than settlement availability unless content-addressed storage is added later;
- the UI must communicate pending versus confirmed transactions.

## Alternatives considered

**Store all metadata on-chain.** Rejected because long descriptions and presentation data do not participate in settlement and impose permanent cost and schema rigidity.

**Treat PostgreSQL balances as authoritative.** Rejected because it recreates a custodial backend and lets database failure or compromise change financial truth.

**Query RPC nodes for every API request.** Rejected because activity feeds, user history, metadata joins, and recovery-oriented queries are inefficient and operationally fragile without an index.

