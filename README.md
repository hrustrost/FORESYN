# Foresyn

Foresyn is a small, production-minded decentralized prediction-market prototype. It is a portfolio project focused on the engineering concerns behind Web3 systems: explicit trust boundaries, safe settlement, restartable blockchain indexing, queryable projections, and honest failure recovery.

This repository is intentionally not a Polymarket clone. The first version will use one simple binary market and a deterministic pooled settlement model. It will not include an order book, custom token, bridge, DAO, or sophisticated oracle network.

## Current status

Foresyn now has a deterministic **contract-to-database-to-REST read path**.

Implemented now:

- a Rust workspace with an Axum health endpoint and scoped market/position read API;
- a React + TypeScript + Vite shell with no wallet or market behavior;
- PostgreSQL-only Docker Compose configuration;
- SQLx migrations for canonical blocks, raw `MarketCreated`/`PositionTaken` logs, durable checkpoints, immutable markets, mutable pool state, and per-user positions;
- a one-shot Rust/Alloy indexer with confirmed historical catch-up, bounded multi-event log queries, typed event decoding, restart-safe checkpoints, and deterministic reorganization rollback/rebuild/replay;
- PostgreSQL-backed `GET /api/markets`, `GET /api/markets/:market_id`, and `GET /api/markets/:market_id/positions` routes;
- a Solidity/Foundry binary pari-mutuel prediction-market contract;
- Rust tests for configuration, decoding, ordering, idempotency, restart, rollback, malformed logs, filtering, and continuity, plus contract unit, fuzz, stateful invariant, reentrancy, and failed-receiver tests;
- architecture, source-of-truth, and settlement-model documentation.

Not implemented yet:

- indexing of resolution, cancellation, and claim events;
- continuous polling or WebSocket subscriptions;
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

See [the architecture document](docs/architecture.md), [ADR 0001](docs/decisions/0001-on-chain-off-chain-boundary.md), [ADR 0002](docs/decisions/0002-settlement-and-claim-model.md), [ADR 0003](docs/decisions/0003-indexer-reliability-model.md), [ADR 0004](docs/decisions/0004-deterministic-reorg-recovery.md), [ADR 0005](docs/decisions/0005-position-projections.md), and [the settlement model](docs/settlement-model.md).

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
curl http://localhost:8080/api/markets
```

The API requires only `DATABASE_URL`, `EVM_CHAIN_ID`, and `FORESYN_CONTRACT_ADDRESS`; it does not require an RPC URL. `FORESYN_BIND_ADDRESS` defaults to `127.0.0.1:8080`, and `FORESYN_CORS_ORIGIN` defaults to the local Vite origin `http://localhost:5173`. CORS permits that explicit origin for read requests and does not enable credentials.

`GET /health` returns `{"status":"ok"}` and reports process health only. The read routes query PostgreSQL projections scoped to the configured chain and contract. They never query RPC per request and never authorize financial writes. Markets are newest-first and support `?limit=`/`?offset=` with a default limit of 20 and maximum of 100.

All chain-derived numeric response fields are decimal strings, preserving values beyond JavaScript's safe integer range. Addresses and metadata digests are lowercase, `0x`-prefixed hex. Resolution, cancellation, and claim events are not indexed, so the API intentionally exposes no lifecycle status.

Run one confirmed historical catch-up after filling every indexer value in `.env`:

```bash
cargo run --locked -p foresyn-backend --bin indexer
```

Required indexer configuration is `DATABASE_URL`, `EVM_RPC_URL`, `EVM_CHAIN_ID`, `FORESYN_CONTRACT_ADDRESS`, `FORESYN_DEPLOYMENT_BLOCK`, `INDEXER_CONFIRMATIONS`, and `INDEXER_BATCH_SIZE`. The command applies embedded SQLx migrations, resumes after the last transactionally committed block, catches up through `latest - confirmations`, then exits. It never scans before the configured deployment block on a fresh database.

An index created by the earlier `MarketCreated`-only version cannot prove historical `PositionTaken` coverage. Its normal startup fails without deleting anything. Rebuild it explicitly from `FORESYN_DEPLOYMENT_BLOCK` with:

```bash
cargo run --locked -p foresyn-backend --bin indexer -- --full-reindex
```

Under the current documented model this clears Foresyn index/projection state for the configured chain, which supports one configured Foresyn contract per chain. Never point this prototype flag at shared or authoritative data.

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

The end-to-end smoke helpers also require Foundry commands on `PATH`. The baseline verifies create/index/restart, the reorg smoke verifies immutable market replacement, the position smoke verifies mutable recovery, and the API smoke verifies the complete event-to-JSON read path:

```powershell
$env:TEST_DATABASE_URL = 'postgres://foresyn:foresyn_dev_only@localhost:5432/foresyn'
.\scripts\anvil-indexer-smoke.ps1
.\scripts\anvil-reorg-smoke.ps1
.\scripts\anvil-position-reorg-smoke.ps1
.\scripts\api-read-smoke.ps1
```

## Security disclaimer

Foresyn is experimental software and has not been audited. It must not be used with real funds. Do not commit private keys, seed phrases, RPC credentials, deployed secrets, or funded wallet details. Local secrets belong in `.env`, which is ignored by Git; `.env.example` contains placeholders only.
