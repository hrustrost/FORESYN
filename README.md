# Foresyn

Foresyn is a small, production-minded decentralized prediction-market prototype. It is a portfolio project focused on the engineering concerns behind Web3 systems: explicit trust boundaries, safe settlement, restartable blockchain indexing, queryable projections, and honest failure recovery.

This repository is intentionally not a Polymarket clone. The first version will use one simple binary market and a deterministic pooled settlement model. It will not include an order book, custom token, bridge, DAO, or sophisticated oracle network.

## Current status

Foresyn is in its **foundation and design** milestone.

Implemented now:

- a Rust workspace with a minimal Axum `GET /health` service;
- a React + TypeScript + Vite shell with no wallet or market behavior;
- PostgreSQL-only Docker Compose configuration;
- an ABI-independent migration for canonical indexed blocks and raw contract logs;
- architecture, source-of-truth, and settlement-model documentation.

Not implemented yet:

- prediction-market contracts;
- blockchain indexer or RPC integration;
- market/query API endpoints beyond health;
- database integration in the backend;
- wallet integration or transaction submission.

## Architecture

```text
React / TypeScript  -->  Rust / Axum  -->  PostgreSQL projections
       |                                      ^
       | wallet transactions                  | decoded, confirmed logs
       v                                      |
EVM prediction-market contract  -->  Rust / Alloy indexer (planned)
```

The chain will be authoritative for market lifecycle, stakes, resolution, and claims. PostgreSQL will store replayable event history and query-optimized projections; it must never become a competing ledger. Descriptive metadata such as market titles and images remains off-chain.

See [the architecture document](docs/architecture.md), [ADR 0001](docs/decisions/0001-on-chain-off-chain-boundary.md), and [the proposed settlement model](docs/settlement-model.md).

## Repository layout

```text
backend/                 Rust/Axum API and versioned SQL migrations
contracts/               Solidity/Foundry project placeholder
docs/                    Architecture, ADRs, and design notes
frontend/                React/TypeScript/Vite application
docker-compose.yml       Local PostgreSQL only
```

The backend and future indexer are intended to remain a modular Rust monolith until operational evidence justifies separate deployable services.

## Technology choices

- **Rust, Tokio, and Axum** for explicit types, predictable performance, and a small HTTP surface.
- **SQLx and PostgreSQL (planned backend integration)** for compile-time-aware SQL and transactional, auditable projections.
- **Alloy (planned)** for EVM RPC, logs, and strongly typed event decoding.
- **Solidity and Foundry (planned)** for contracts and invariant-oriented tests.
- **React, TypeScript, and Vite** for a small wallet-facing client.

Dependencies are added when they are used. For example, SQLx and Alloy are not yet Rust dependencies because this milestone does not connect to the database or chain.

## Local development

Prerequisites:

- Rust 1.85 or newer;
- Node.js 20.19+ or 22.12+;
- Docker with Compose;
- Foundry only when beginning contract development.

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

Run the frontend:

```bash
cd frontend
npm install
npm run dev
```

Useful verification commands:

```bash
cargo fmt --all --check
cargo test --workspace
cd frontend && npm run build
docker compose config
```

The initial SQL migration is version-controlled under `backend/migrations`, but nothing runs it automatically yet. Migration execution will be added with the first SQLx-backed indexer slice.

## Security disclaimer

Foresyn is experimental software and has not been audited. It must not be used with real funds. Do not commit private keys, seed phrases, RPC credentials, deployed secrets, or funded wallet details. Local secrets belong in `.env`, which is ignored by Git; `.env.example` contains placeholders only.

