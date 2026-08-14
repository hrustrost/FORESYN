CREATE TABLE indexer_checkpoints (
    chain_id BIGINT NOT NULL CHECK (chain_id > 0),
    contract_address BYTEA NOT NULL CHECK (octet_length(contract_address) = 20),
    last_block_number BIGINT NOT NULL CHECK (last_block_number >= 0),
    last_block_hash BYTEA NOT NULL CHECK (octet_length(last_block_hash) = 32),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (chain_id, contract_address),
    FOREIGN KEY (chain_id, last_block_number, last_block_hash)
        REFERENCES indexed_blocks (chain_id, block_number, block_hash)
        ON DELETE CASCADE
);

CREATE TABLE markets (
    chain_id BIGINT NOT NULL CHECK (chain_id > 0),
    contract_address BYTEA NOT NULL CHECK (octet_length(contract_address) = 20),
    market_id NUMERIC(78, 0) NOT NULL CHECK (market_id >= 0),
    resolver BYTEA NOT NULL CHECK (octet_length(resolver) = 20),
    creator BYTEA NOT NULL CHECK (octet_length(creator) = 20),
    deadline NUMERIC(20, 0) NOT NULL CHECK (deadline >= 0),
    metadata_digest BYTEA NOT NULL CHECK (octet_length(metadata_digest) = 32),
    creation_block_number BIGINT NOT NULL CHECK (creation_block_number >= 0),
    creation_transaction_hash BYTEA NOT NULL
        CHECK (octet_length(creation_transaction_hash) = 32),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (chain_id, contract_address, market_id),
    FOREIGN KEY (chain_id, creation_block_number)
        REFERENCES indexed_blocks (chain_id, block_number)
        ON DELETE CASCADE
);

CREATE INDEX markets_creator_idx
    ON markets (chain_id, contract_address, creator);

CREATE INDEX markets_deadline_idx
    ON markets (chain_id, contract_address, deadline);
