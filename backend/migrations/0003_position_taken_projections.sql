-- PositionTaken introduces mutable read models. Their values are reconstructed
-- from retained canonical raw events after a reorg; updated_block_number is
-- provenance, not a cascade-based rollback mechanism.

CREATE TABLE market_states (
    chain_id BIGINT NOT NULL CHECK (chain_id > 0),
    contract_address BYTEA NOT NULL CHECK (octet_length(contract_address) = 20),
    market_id NUMERIC(78, 0) NOT NULL CHECK (market_id >= 0),
    yes_pool NUMERIC(78, 0) NOT NULL CHECK (yes_pool >= 0),
    no_pool NUMERIC(78, 0) NOT NULL CHECK (no_pool >= 0),
    updated_block_number BIGINT NOT NULL CHECK (updated_block_number >= 0),
    PRIMARY KEY (chain_id, contract_address, market_id),
    FOREIGN KEY (chain_id, contract_address, market_id)
        REFERENCES markets (chain_id, contract_address, market_id)
        ON DELETE CASCADE
);

CREATE TABLE market_positions (
    chain_id BIGINT NOT NULL CHECK (chain_id > 0),
    contract_address BYTEA NOT NULL CHECK (octet_length(contract_address) = 20),
    market_id NUMERIC(78, 0) NOT NULL CHECK (market_id >= 0),
    user_address BYTEA NOT NULL CHECK (octet_length(user_address) = 20),
    yes_stake NUMERIC(78, 0) NOT NULL CHECK (yes_stake >= 0),
    no_stake NUMERIC(78, 0) NOT NULL CHECK (no_stake >= 0),
    updated_block_number BIGINT NOT NULL CHECK (updated_block_number >= 0),
    PRIMARY KEY (chain_id, contract_address, market_id, user_address),
    FOREIGN KEY (chain_id, contract_address, market_id)
        REFERENCES markets (chain_id, contract_address, market_id)
        ON DELETE CASCADE
);

CREATE INDEX market_positions_user_idx
    ON market_positions (chain_id, contract_address, user_address);

-- This is a coverage marker, not a second checkpoint. It proves that the
-- configured contract was indexed with PositionTaken enabled from this block.
CREATE TABLE indexer_contract_coverage (
    chain_id BIGINT NOT NULL CHECK (chain_id > 0),
    contract_address BYTEA NOT NULL CHECK (octet_length(contract_address) = 20),
    position_taken_from_block BIGINT NOT NULL CHECK (position_taken_from_block >= 0),
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (chain_id, contract_address)
);
