# Contracts

This directory is reserved for the Solidity/Foundry project.

Foundry was not available when the repository foundation was created, so no generated project or unverified contract stub is committed. Before adding business logic:

1. review `docs/settlement-model.md`;
2. install Foundry and initialize this directory without a sample Counter contract;
3. define the market state machine, events, custom errors, and storage layout;
4. write lifecycle, authorization, rounding, claim, and solvency tests;
5. implement the smallest contract that satisfies those tests.

No private keys, RPC credentials, deployment broadcasts, or funded wallet material belong in this repository.

