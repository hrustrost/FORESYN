-- This migration stores canonical chain input only. Contract-specific market and
-- position projections are intentionally deferred until the event ABI is fixed.

CREATE TABLE indexed_blocks (
    chain_id BIGINT NOT NULL CHECK (chain_id > 0),
    block_number BIGINT NOT NULL CHECK (block_number >= 0),
    block_hash BYTEA NOT NULL CHECK (octet_length(block_hash) = 32),
    parent_hash BYTEA NOT NULL CHECK (octet_length(parent_hash) = 32),
    block_timestamp TIMESTAMPTZ NOT NULL,
    indexed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (chain_id, block_number),
    UNIQUE (chain_id, block_hash),
    UNIQUE (chain_id, block_number, block_hash)
);

CREATE TABLE blockchain_events (
    chain_id BIGINT NOT NULL CHECK (chain_id > 0),
    block_number BIGINT NOT NULL CHECK (block_number >= 0),
    block_hash BYTEA NOT NULL CHECK (octet_length(block_hash) = 32),
    transaction_hash BYTEA NOT NULL CHECK (octet_length(transaction_hash) = 32),
    transaction_index INTEGER NOT NULL CHECK (transaction_index >= 0),
    log_index INTEGER NOT NULL CHECK (log_index >= 0),
    contract_address BYTEA NOT NULL CHECK (octet_length(contract_address) = 20),
    topics BYTEA[] NOT NULL CHECK (cardinality(topics) BETWEEN 1 AND 4),
    data BYTEA NOT NULL,
    indexed_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (chain_id, transaction_hash, log_index),
    UNIQUE (chain_id, block_number, log_index),
    FOREIGN KEY (chain_id, block_number, block_hash)
        REFERENCES indexed_blocks (chain_id, block_number, block_hash)
        ON DELETE CASCADE
);

CREATE INDEX blockchain_events_contract_block_idx
    ON blockchain_events (chain_id, contract_address, block_number, log_index);

