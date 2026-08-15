use alloy::{
    primitives::{Address, B256, U256},
    sol,
    sol_types::{Error as SolTypeError, SolEvent},
};
use thiserror::Error;

sol! {
    /// Exact enum used by ForesynPredictionMarket.PositionTaken.
    enum Outcome {
        Unset,
        Yes,
        No
    }

    /// Exact event emitted by ForesynPredictionMarket.createMarket.
    event MarketCreated(
        uint256 indexed marketId,
        address indexed resolver,
        address creator,
        uint64 deadline,
        bytes32 metadataDigest
    );

    /// Exact event emitted by ForesynPredictionMarket.takePosition.
    event PositionTaken(
        uint256 indexed marketId,
        address indexed user,
        Outcome outcome,
        uint256 amount,
        uint256 userOutcomeStake,
        uint256 yesPool,
        uint256 noPool
    );
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarketCreatedProjection {
    pub market_id: U256,
    pub resolver: Address,
    pub creator: Address,
    pub deadline: u64,
    pub metadata_digest: B256,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinaryOutcome {
    Yes,
    No,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PositionTakenProjection {
    pub market_id: U256,
    pub user: Address,
    pub outcome: BinaryOutcome,
    pub amount: U256,
    pub user_outcome_stake: U256,
    pub yes_pool: U256,
    pub no_pool: U256,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodedEvent {
    MarketCreated(MarketCreatedProjection),
    PositionTaken(PositionTakenProjection),
}

#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("log is missing the {event} signature topic")]
    WrongSignature { event: &'static str },
    #[error("malformed {event} log: {source}")]
    Malformed {
        event: &'static str,
        #[source]
        source: SolTypeError,
    },
    #[error("PositionTaken contains an invalid or non-binary outcome")]
    NonBinaryPositionOutcome,
}

pub const fn market_created_topic() -> B256 {
    MarketCreated::SIGNATURE_HASH
}

pub const fn position_taken_topic() -> B256 {
    PositionTaken::SIGNATURE_HASH
}

pub fn decode_known_event(
    topics: &[B256],
    data: &[u8],
) -> Result<Option<DecodedEvent>, DecodeError> {
    match topics.first() {
        Some(topic) if *topic == market_created_topic() => decode_market_created(topics, data)
            .map(|event| Some(DecodedEvent::MarketCreated(event))),
        Some(topic) if *topic == position_taken_topic() => decode_position_taken(topics, data)
            .map(|event| Some(DecodedEvent::PositionTaken(event))),
        _ => Ok(None),
    }
}

pub fn decode_market_created(
    topics: &[B256],
    data: &[u8],
) -> Result<MarketCreatedProjection, DecodeError> {
    if topics.first() != Some(&MarketCreated::SIGNATURE_HASH) {
        return Err(DecodeError::WrongSignature {
            event: "MarketCreated",
        });
    }

    let event =
        MarketCreated::decode_raw_log_validate(topics.iter().copied(), data).map_err(|source| {
            DecodeError::Malformed {
                event: "MarketCreated",
                source,
            }
        })?;

    Ok(MarketCreatedProjection {
        market_id: event.marketId,
        resolver: event.resolver,
        creator: event.creator,
        deadline: event.deadline,
        metadata_digest: event.metadataDigest,
    })
}

pub fn decode_position_taken(
    topics: &[B256],
    data: &[u8],
) -> Result<PositionTakenProjection, DecodeError> {
    if topics.first() != Some(&PositionTaken::SIGNATURE_HASH) {
        return Err(DecodeError::WrongSignature {
            event: "PositionTaken",
        });
    }

    let event =
        PositionTaken::decode_raw_log_validate(topics.iter().copied(), data).map_err(|source| {
            DecodeError::Malformed {
                event: "PositionTaken",
                source,
            }
        })?;
    let outcome = match event.outcome {
        Outcome::Yes => BinaryOutcome::Yes,
        Outcome::No => BinaryOutcome::No,
        Outcome::Unset | Outcome::__Invalid => {
            return Err(DecodeError::NonBinaryPositionOutcome);
        }
    };

    Ok(PositionTakenProjection {
        market_id: event.marketId,
        user: event.user,
        outcome,
        amount: event.amount,
        user_outcome_stake: event.userOutcomeStake,
        yes_pool: event.yesPool,
        no_pool: event.noPool,
    })
}

#[cfg(test)]
mod tests {
    use alloy::{
        primitives::{Address, B256, U256},
        sol_types::SolEvent,
    };

    use super::{
        BinaryOutcome, DecodeError, MarketCreated, Outcome, PositionTaken, decode_market_created,
        decode_position_taken,
    };

    #[test]
    fn decodes_market_created_indexed_topics_and_non_indexed_data() {
        let expected = MarketCreated {
            marketId: U256::from(42),
            resolver: Address::repeat_byte(0x11),
            creator: Address::repeat_byte(0x22),
            deadline: 1_900_000_000,
            metadataDigest: B256::repeat_byte(0x33),
        };
        let encoded = expected.encode_log_data();

        let decoded = decode_market_created(encoded.topics(), &encoded.data).unwrap();

        assert_eq!(decoded.market_id, expected.marketId);
        assert_eq!(decoded.resolver, expected.resolver);
        assert_eq!(decoded.creator, expected.creator);
        assert_eq!(decoded.deadline, expected.deadline);
        assert_eq!(decoded.metadata_digest, expected.metadataDigest);
    }

    #[test]
    fn decodes_position_taken_exact_abi() {
        let expected = PositionTaken {
            marketId: U256::from(42),
            user: Address::repeat_byte(0x44),
            outcome: Outcome::Yes,
            amount: U256::from(2),
            userOutcomeStake: U256::from(7),
            yesPool: U256::from(11),
            noPool: U256::from(13),
        };
        let encoded = expected.encode_log_data();

        let decoded = decode_position_taken(encoded.topics(), &encoded.data).unwrap();

        assert_eq!(decoded.market_id, expected.marketId);
        assert_eq!(decoded.user, expected.user);
        assert_eq!(decoded.outcome, BinaryOutcome::Yes);
        assert_eq!(decoded.amount, expected.amount);
        assert_eq!(decoded.user_outcome_stake, expected.userOutcomeStake);
        assert_eq!(decoded.yes_pool, expected.yesPool);
        assert_eq!(decoded.no_pool, expected.noPool);
    }

    #[test]
    fn rejects_unset_position_outcome() {
        let event = PositionTaken {
            marketId: U256::from(1),
            user: Address::repeat_byte(0x44),
            outcome: Outcome::Unset,
            amount: U256::from(1),
            userOutcomeStake: U256::from(1),
            yesPool: U256::from(1),
            noPool: U256::ZERO,
        };
        let encoded = event.encode_log_data();

        assert!(matches!(
            decode_position_taken(encoded.topics(), &encoded.data),
            Err(DecodeError::NonBinaryPositionOutcome)
        ));
    }

    #[test]
    fn rejects_wrong_signature_and_malformed_data() {
        let wrong_topics = [B256::repeat_byte(0xff)];
        assert!(matches!(
            decode_market_created(&wrong_topics, &[]),
            Err(DecodeError::WrongSignature {
                event: "MarketCreated"
            })
        ));

        let malformed_topics = [MarketCreated::SIGNATURE_HASH, B256::ZERO, B256::ZERO];
        assert!(matches!(
            decode_market_created(&malformed_topics, &[0; 31]),
            Err(DecodeError::Malformed {
                event: "MarketCreated",
                ..
            })
        ));
    }
}
