CREATE TABLE market_metadata (
    chain_id BIGINT NOT NULL CHECK (chain_id > 0),
    contract_address BYTEA NOT NULL CHECK (octet_length(contract_address) = 20),
    market_id NUMERIC(78, 0) NOT NULL CHECK (market_id >= 0),
    question TEXT NOT NULL,
    description TEXT NOT NULL,
    resolution_criteria TEXT NOT NULL,
    category TEXT NOT NULL,
    source_url TEXT,
    metadata_digest BYTEA NOT NULL CHECK (octet_length(metadata_digest) = 32),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (chain_id, contract_address, market_id),
    FOREIGN KEY (chain_id, contract_address, market_id)
        REFERENCES markets (chain_id, contract_address, market_id)
        ON DELETE CASCADE
);

CREATE INDEX market_metadata_chain_idx
    ON market_metadata (chain_id, contract_address);
