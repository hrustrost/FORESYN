# Foresyn contracts

This Foundry project contains the on-chain settlement component of Foresyn. It intentionally implements one binary, native-ETH, pari-mutuel market rather than an order book, AMM, transferable position token, or oracle network.

## Contract boundary

`ForesynPredictionMarket` stores only settlement-relevant data:

- sequential market ID and a `bytes32` commitment to the off-chain market rules;
- deadline, lifecycle, assigned resolver, and winning outcome;
- YES/NO pools and each wallet's YES/NO stake;
- claim state and cumulative settlement accounting.

Titles, descriptions, images, categories, and activity views remain off-chain. The metadata digest commits the on-chain market to an exact off-chain question/rules document without paying to store that document in contract storage.

The owner may create markets and pause new creation/staking. Each market has its own resolver, which may resolve or cancel that market only at/after its deadline. Pausing deliberately does not block resolution or claims, so the owner cannot use the emergency stop to freeze already-deposited funds.

See [`docs/settlement-model.md`](../docs/settlement-model.md) and [ADR 0002](../docs/decisions/0002-settlement-and-claim-model.md) for the formula and trust assumptions.

## Dependencies

- Solidity 0.8.20
- Foundry / forge-std v1.16.2
- OpenZeppelin Contracts v5.6.1: `Ownable`, `Pausable`, `ReentrancyGuard`, and `Math`

Dependencies are pinned as Git submodules. After cloning the repository:

```bash
git submodule update --init --recursive
```

## Verification

From this directory:

```bash
forge fmt --check
forge build
forge test -vvv
forge test --gas-report
```

This code is experimental and unaudited. Do not use it with real funds.

## Events for the future indexer

`marketId` and the relevant `user` or `resolver` are indexed because those are expected filter keys. Outcomes and amounts remain event data to avoid spending topics on low-selectivity values.

- `MarketCreated` includes resolver, creator, deadline, and metadata digest.
- `PositionTaken` includes amount, updated user-side stake, and both updated pools.
- `MarketResolved` includes outcome, total pool, and winning pool.
- `MarketCancelled` includes cancellation reason, attempted outcome, and total pool.
- `WinningsClaimed` and `RefundClaimed` include the transferred amount.

These payloads let the planned Rust indexer update projections without making an RPC state read after every log.
