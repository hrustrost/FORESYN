# FORESYN

### EVM Prediction Market & Rust Blockchain Infrastructure
[![CI](https://github.com/hrustrost/FORESYN/actions/workflows/ci.yml/badge.svg)](https://github.com/hrustrost/FORESYN/actions/workflows/ci.yml)

FORESYN is an EVM prediction-market prototype built to explore reliable blockchain backend architecture beyond basic event listening.

The system combines Solidity smart contracts with a Rust/Alloy blockchain indexer, deterministic EVM reorganization recovery, PostgreSQL projections, an Axum REST API and a React/MetaMask Web3 client.

The blockchain remains the financial source of truth. PostgreSQL stores replayable, query-optimized projections that can be deterministically rebuilt from canonical blockchain history.

## Architecture Overview

```mermaid
flowchart LR
    U[React Client]
    W[MetaMask / EIP-1193]
    C[Solidity Contract]
    I[Rust / Alloy Indexer]
    P[(PostgreSQL)]
    A[Axum REST API]

    U -->|sign transaction| W
    W -->|takePosition| C
    C -->|EVM events| I
    I --> P
    P --> A
    A --> U
```

### Write path

**React → MetaMask → user signature → Solidity → EVM**

Financial transactions are signed directly by the user's wallet. The backend never receives or manages user private keys.

### Read path

**EVM events → Rust/Alloy indexer → PostgreSQL → Axum REST API → React**

The indexer maintains durable checkpoints, validates canonical chain state and deterministically recovers from EVM reorganizations.

## Engineering Highlights

- Solidity binary prediction-market contract with pull-based settlement
- Rust, Tokio and Axum backend
- Alloy-based EVM event indexing
- deterministic reorg detection, rollback and canonical replay
- transactional PostgreSQL projections with SQLx
- idempotent event processing and durable checkpoints
- direct MetaMask / EIP-1193 transaction signing
- exact `uint256` handling without JavaScript floating-point conversion
- Foundry unit, fuzz and stateful invariant testing
- PostgreSQL integration and Anvil end-to-end smoke testing

## Current status

Foresyn now has a deterministic indexed read path and a user-signed Web3 write path.

Implemented now:

- a Rust workspace with an Axum health endpoint and scoped market/position read API;
- a responsive React + TypeScript market dashboard with exact wei formatting and minimal injected-wallet support;
- PostgreSQL-only Docker Compose configuration;
- SQLx migrations for canonical blocks, raw `MarketCreated`/`PositionTaken` logs, durable checkpoints, immutable markets, mutable pool state, and per-user positions;
- a one-shot Rust/Alloy indexer with confirmed historical catch-up, bounded multi-event log queries, typed event decoding, restart-safe checkpoints, and deterministic reorganization rollback/rebuild/replay;
- an explicit indexer watch mode that repeatedly invokes the same catch-up algorithm;
- PostgreSQL-backed `GET /api/markets`, `GET /api/markets/:market_id`, and `GET /api/markets/:market_id/positions` routes;
- direct user-signed `takePosition` writes through MetaMask/EIP-1193, followed by bounded REST polling until the projection updates;
- a Solidity/Foundry binary pari-mutuel prediction-market contract;
- Rust tests for configuration, decoding, ordering, idempotency, restart, rollback, malformed logs, filtering, and continuity, plus contract unit, fuzz, stateful invariant, reentrancy, and failed-receiver tests;
- architecture, source-of-truth, and settlement-model documentation.

Not implemented yet:

- indexing of resolution, cancellation, and claim events;
- WebSocket subscriptions;

## Architecture

```text
WRITE: React -> injected EIP-1193 wallet -> user signature -> Solidity contract
                                                               |
                                                               | PositionTaken
                                                               v
READ:  React <- Axum REST API <- PostgreSQL <- Rust/Alloy indexer <- EVM events
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

One-shot remains the default. For the interactive demo, repeat the same catch-up
algorithm every two seconds with:

```bash
cargo run --locked -p foresyn-backend --bin indexer -- --watch
```

`--poll-interval-ms 2000` optionally changes the watch interval. A fatal RPC,
database, decode, or invariant error terminates the process instead of being hidden.
`--full-reindex --watch` performs the explicit destructive reindex once during safe
startup, then enters normal watch iterations; it never repeats the reindex.

An index created by the earlier `MarketCreated`-only version cannot prove historical `PositionTaken` coverage. Its normal startup fails without deleting anything. Rebuild it explicitly from `FORESYN_DEPLOYMENT_BLOCK` with:

```bash
cargo run --locked -p foresyn-backend --bin indexer -- --full-reindex
```

Under the current documented model this clears Foresyn index/projection state for the configured chain, which supports one configured Foresyn contract per chain. Never point this prototype flag at shared or authoritative data.

Run the frontend:

```bash
cd frontend
npm install
cp .env.example .env
npm run dev
```

On PowerShell, use `Copy-Item .env.example .env`. Configure the frontend with the
same chain and deployed contract used by the indexer:

```dotenv
VITE_API_URL=http://localhost:8080
VITE_CHAIN_ID=31337
VITE_CONTRACT_ADDRESS=0x_REPLACE_WITH_DEPLOYED_ADDRESS
VITE_CHAIN_NAME=Anvil
VITE_WALLET_RPC_URL=http://127.0.0.1:8545
```

Application reads still go only through Axum/PostgreSQL. Wallet RPC is used only
for account/network state, client-side signing, and submitting `takePosition`
directly to Solidity. After mining, the frontend polls the REST projection and does
not report indexing success until the updated pool and user stake are observable.

For a complete local demo:

1. Start Anvil with `anvil --chain-id 31337`.
2. Deploy `ForesynPredictionMarket` with `forge create`, using an Anvil development
   account as owner, and copy the deployed address.
3. Start PostgreSQL with `docker compose up -d postgres`.
4. Put the RPC, chain, contract, deployment block, and database values in the root
   `.env`, then run the indexer with `--watch`.
5. Run the Axum API with `cargo run -p foresyn-backend`.
6. Put the matching values in `frontend/.env`, then run `npm run dev`.
7. Add the local Anvil network to MetaMask and connect a funded Anvil development
   account. The private key is imported into the wallet for local use only and is
   never given to the React application or backend.

The only frontend write is a direct wallet-signed `takePosition` call. The product
has no settlement controls and Axum exposes no financial transaction endpoint.

Useful verification commands:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd frontend && npm run build && npm run lint && npm test
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
.\scripts\anvil-watch-roundtrip-smoke.ps1
```

The final watch smoke starts the indexer before creating the market, sends a later
position transaction, and proves one continuously running indexer process updates
the REST projection without a restart.

## Security disclaimer

Foresyn is experimental software and has not been audited. It must not be used with real funds. Do not commit private keys, seed phrases, RPC credentials, deployed secrets, or funded wallet details. Local secrets belong in `.env`, which is ignored by Git; `.env.example` contains placeholders only.

Wallets sign financial writes client-side; no private key is sent to Axum. Frontend
validation and wrong-network disabling are user-safety features, not a security
boundary. The Solidity contract enforces financial rules, the blockchain remains
the financial source of truth, and PostgreSQL projections remain disposable and
rebuildable.
