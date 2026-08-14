# ADR 0002: Use pooled settlement with pull-based claims

- Status: Accepted
- Date: 2026-08-14

## Context

The first Foresyn contract needs financially correct binary settlement without introducing an order book, automated market maker, custom token, or price model. It must also be small enough to audit and explain line-by-line.

## Decision

Each market has YES and NO native-ETH pools. A wallet may stake either or both sides, and repeated stakes are aggregated. After the deadline, the market's assigned resolver selects an outcome.

For a resolved market with total pool `T`, winning pool `W`, and a user's winning stake `s`, the normal payout is:

```text
floor(s * T / W)
```

The contract uses OpenZeppelin `Math.mulDiv` so the intermediate multiplication cannot overflow. It tracks claimed winning stake and cumulative payout. The claimant who accounts for the last unclaimed winning stake receives `T - claimedAmount`, assigning all remaining rounding wei and leaving no settlement dust. Claim order can affect only that integer remainder, never total liabilities.

If the resolver selects a side with zero stake, the contract transitions to `Cancelled`. Every participant may then withdraw the exact sum of their YES and NO stakes. This avoids division by zero and prevents the non-empty pool from becoming trapped.

Claims are pull-based: each participant calls `claim`. Claim state and accounting are updated before native ETH is sent, the function is non-reentrant, and a failed transfer reverts the whole transaction so the claim remains retryable.

The prototype uses native ETH because it avoids approvals, a custom token, and non-standard ERC-20 transfer behavior. This is suitable for local/test networks, not an assertion that volatile native currency is appropriate for a production prediction market.

The contract owner creates markets and may pause only new creation and staking. Every market stores its own resolver. At/after the deadline, that resolver may resolve or cancel an unresolvable market. Resolution and claims remain callable while paused, preventing the owner from using pause authority to rewrite outcomes or freeze existing withdrawal rights.

A non-zero `bytes32 metadataDigest` commits each on-chain market to an exact off-chain question and resolution-rules document. The full text remains off-chain; the digest provides integrity but not availability.

## Consequences

Benefits:

- settlement is deterministic, solvent, and straightforward to test;
- no winner enumeration is required;
- pull payments isolate receiver failures to that receiver's claim;
- raw events contain enough state changes for future PostgreSQL projections;
- per-market resolvers make the trust relationship explicit.

Costs and trust assumptions:

- odds are unknown until betting closes because this is pari-mutuel, not fixed-price trading;
- the final claimant may receive a few additional wei of rounding remainder;
- the assigned resolver is centralized and can report an incorrect real-world outcome;
- the owner can stop new deposits but cannot cancel markets unless it is separately assigned as resolver;
- a resolver may choose cancellation after the deadline, so its honesty remains a trust assumption;
- block producers can influence timestamps slightly, so markets must not depend on second-level deadline precision;
- forced ETH is not part of any market pool and has no administrative sweep path;
- the metadata document must remain available off-chain.

## Production limitations

A production system would require a stronger oracle and dispute process, an audited stable settlement asset, explicit operational key management, independent contract audits, carefully designed cancellation/dispute rules, and likely time-delayed or multi-party administration. Those mechanisms are intentionally outside this prototype.
