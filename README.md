# Foresyn

Foresyn is a small, production-minded decentralized prediction-market prototype. It is a portfolio project focused on the engineering concerns behind Web3 systems: explicit trust boundaries, safe settlement, restartable blockchain indexing, queryable projections, and honest failure recovery.

This repository is intentionally not a Polymarket clone. The first version will use one simple binary market and a deterministic pooled settlement model. It will not include an order book, custom token, bridge, DAO, or sophisticated oracle network.

## Current status

Foresyn has completed its first **contract-to-database indexing** vertical slice.

Implemented now:

- a Rust workspace with a minimal Axum `GET /health` service;
- a React + TypeScript + Vite shell with no wallet or market behavior;
- PostgreSQL-only Docker Compose configuration;
- SQLx migrations for canonical blocks, raw contract logs, durable checkpoints, and a `MarketCreated` projection;
- a one-shot Rust/Alloy indexer with confirmed historical catch-up, bounded log ranges, typed event decoding, restart-safe checkpoints, and parent-hash reorganization detection;
- a Solidity/Foundry binary pari-mutuel prediction-market contract;
- Rust tests for configuration, decoding, ordering, idempotency, restart, rollback, malformed logs, filtering, and continuity, plus contract unit, fuzz, stateful invariant, reentrancy, and failed-receiver tests;
- architecture, source-of-truth, and settlement-model documentation.

Not implemented yet:

- market/query API endpoints beyond health;
- indexing of contract events other than `MarketCreated`;
- reorganization rollback/replay, continuous polling, or WebSocket subscriptions;
- wallet integration or transaction submission.

## Architecture

```text
React / TypeScript  -->  Rust / Axum  -->  PostgreSQL projections
       |                                      ^
       | wallet transactions                  | decoded, confirmed logs
       v                                      |
EVM prediction-market contract  -->  Rust / Alloy indexer
```

The chain will be authoritative for market lifecycle, stakes, resolution, and claims. PostgreSQL will store replayable event history and query-optimized projections; it must never become a competing ledger. Descriptive metadata such as market titles and images remains off-chain.

See [the architecture document](docs/architecture.md), [ADR 0001](docs/decisions/0001-on-chain-off-chain-boundary.md), [ADR 0002](docs/decisions/0002-settlement-and-claim-model.md), [ADR 0003](docs/decisions/0003-indexer-reliability-model.md), and [the settlement model](docs/settlement-model.md).

## Repository layout

```text
backend/                 Rust/Axum API, Alloy indexer, and SQLx migrations
contracts/               Implemented Solidity/Foundry settlement contract and tests
docs/                    Architecture, ADRs, and design notes
frontend/                React/TypeScript/Vite application
scripts/                 Local integration/smoke-test helpers
docker-compose.yml       Local PostgreSQL only
```

The backend and indexer remain one modular Rust crate with separate binaries until operational evidence justifies separate deployable services.

## Technology choices

- **Rust, Tokio, and Axum** for explicit types, predictable performance, and a small HTTP surface.
- **SQLx and PostgreSQL** for versioned migrations and transactional, auditable projections.
- **Alloy** for EVM RPC, address/hash types, topic filtering, and strongly typed event decoding.
- **Solidity and Foundry** for explicit contract state transitions and invariant-oriented tests.
- **React, TypeScript, and Vite** for a small wallet-facing client.

Dependencies are added when they are used. This slice pins Alloy 0.15 and SQLx 0.8 to stay within the repository's Rust compatibility target instead of pulling newer releases with higher minimum compiler versions.

The contract uses OpenZeppelin only for ownership, narrow emergency pausing, reentrancy protection, and full-precision payout arithmetic. Market state and settlement logic remain explicit in the Foresyn contract.

## Local development

Prerequisites:

- Rust 1.85 or newer;
- Node.js 20.19+ or 22.12+;
- Docker with Compose;
- Foundry for contract work and the optional Anvil smoke test.

Create local configuration:

```bash
cp .env.example .env
```

On PowerShell, use `Copy-Item .env.example .env` instead.

Start PostgreSQL:

```bash
docker compose up -d postgres
docker compose ps
```

Run the backend:

```bash
cargo run -p foresyn-backend
curl http://localhost:8080/health
```

The response is `{"status":"ok"}`. The current health check reports process health only; it does not claim database or chain readiness.

Run one confirmed historical catch-up after filling every indexer value in `.env`:

```bash
cargo run --locked -p foresyn-backend --bin indexer
```

Required indexer configuration is `DATABASE_URL`, `EVM_RPC_URL`, `EVM_CHAIN_ID`, `FORESYN_CONTRACT_ADDRESS`, `FORESYN_DEPLOYMENT_BLOCK`, `INDEXER_CONFIRMATIONS`, and `INDEXER_BATCH_SIZE`. The command applies embedded SQLx migrations, resumes after the last transactionally committed block, catches up through `latest - confirmations`, then exits. It never scans before the configured deployment block on a fresh database.

Run the frontend:

```bash
cd frontend
npm install
npm run dev
```

Useful verification commands:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd frontend && npm run build
docker compose config
```

Run contract verification after initializing Git submodules:

```bash
git submodule update --init --recursive
cd contracts
forge fmt --check
forge build
forge test -vvv
forge test --gas-report
```

Database integration tests run when `TEST_DATABASE_URL` points to a disposable PostgreSQL database; otherwise the database-specific test prints a skip reason. The test truncates Foresyn indexing tables, so never point it at shared or production data.

The end-to-end smoke helper also requires Foundry commands on `PATH`. It starts Anvil, deploys the contract, creates one market, runs the indexer twice, and verifies that raw and projected rows remain singular after restart:

```powershell
$env:TEST_DATABASE_URL = 'postgres://foresyn:foresyn_dev_only@localhost:5432/foresyn'
.\scripts\anvil-indexer-smoke.ps1
```

## Security disclaimer

Foresyn is experimental software and has not been audited. It must not be used with real funds. Do not commit private keys, seed phrases, RPC credentials, deployed secrets, or funded wallet details. Local secrets belong in `.env`, which is ignored by Git; `.env.example` contains placeholders only.
