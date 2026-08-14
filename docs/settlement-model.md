# Proposed MVP settlement model

Status: implemented by `contracts/src/ForesynPredictionMarket.sol` and retained as the settlement specification.

## Goal

Use the smallest economic model that is solvent, deterministic from chain state, easy to test, and understandable without an order book or automated market maker.

## Asset and positions

The prototype accepts the chain's native currency. This avoids token approvals, non-standard ERC-20 behavior, and a custom token. It is appropriate only for local/test networks; a production market would normally need an explicitly supported, audited stable asset.

Each market has two outcomes, `YES` and `NO`. While the market is open, an address may add stake to either or both outcomes. Stakes are aggregated by `(market, address, outcome)`.

There are no quoted prices and no promise of fixed odds. This is a pooled pari-mutuel model: the winning side divides the complete market pool in proportion to winning stake.

## Lifecycle

```text
Open --authorized resolution after deadline--> Resolved(YES or NO)
Open --zero winning pool / emergency cancellation--> Cancelled
Resolved or Cancelled --claims--> remains terminal
```

- Creation requires a future deadline.
- Positions are accepted only in `Open` and strictly before the deadline.
- Only an authorized resolver may resolve, and only at or after the deadline.
- Resolution is final in the MVP; the resolver trust assumption must be visible to users.
- If the selected outcome has no stake, the market becomes `Cancelled` and all stakes are refundable. This prevents trapped funds and nonsensical division by zero.
- A narrowly authorized emergency cancellation mechanism may be included, but its conditions and events must be explicit.

The implemented prototype automatically cancels a zero-winning-pool resolution. It also permits the assigned resolver to cancel at/after the deadline when the real-world question cannot be resolved reliably. The owner has no separate cancellation power, and terminal markets cannot be changed.

## Payout

Let:

- `Y` be total YES stake;
- `N` be total NO stake;
- `T = Y + N` be the complete pool;
- `W` be total stake on the resolved winning outcome;
- `s(a)` be address `a`'s winning stake.

For every winner except the final winning-stake claimant:

```text
payout(a) = floor(s(a) * T / W)
```

The implementation should use full-precision multiplication/division to avoid intermediate overflow. It tracks `claimedWinningStake` and `claimedPayout`. When a claim makes `claimedWinningStake == W`, that final claimant receives:

```text
payout(final) = T - claimedPayout
```

This assigns all integer rounding remainder to the final claimant. Claim ordering can change only the sub-wei rounding allocation; total payout and solvency are invariant. The alternative would be permanently trapped dust or an enumerable winner set, both worse for this MVP.

For a cancelled market, each address receives exactly its combined YES and NO stake.

Claims use pull payments. Claim state and accounting are updated before transferring value, and the claim entry point is protected against reentrancy. An address can claim at most once per market.

The owner may pause creation and new positions during an incident. Resolution and claims remain available while paused so already-deposited funds cannot be administratively frozen through the pause mechanism.

## Invariants

Contract tests must demonstrate at least these properties:

1. `T == Y + N` after every accepted position.
2. Contract liabilities for a market never exceed that market's deposited pool.
3. No position is accepted at or after the deadline or after a terminal state.
4. Only the authorized resolver can create a terminal resolution.
5. No address can successfully claim twice.
6. A losing address receives zero from a resolved market.
7. A cancelled market refunds exactly the caller's recorded stake.
8. Before the final winning claim, cumulative payouts are at most `T`.
9. After all winning stake has claimed, cumulative payouts equal `T` exactly.
10. Resolution with a zero winning pool cannot divide by zero or trap the opposing pool; it follows cancellation refunds.
11. External value transfer occurs only after claim eligibility and accounting are finalized.
12. Pausing and authorization cannot change an already resolved outcome or rewrite stake balances.

## Explicit non-goals

- pricing shares before resolution;
- matching counterparties;
- trading or transferring positions;
- fees, liquidity incentives, or protocol tokenomics;
- decentralized oracle design;
- governance or dispute arbitration.

The next contract-design step should turn these rules into a small state machine and event/error interface before writing storage or transfer logic.
