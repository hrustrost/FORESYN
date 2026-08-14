use alloy::{
    primitives::{Address, B256, U256},
    sol,
    sol_types::{Error as SolTypeError, SolEvent},
};
use thiserror::Error;

sol! {
    /// Exact event emitted by ForesynPredictionMarket.createMarket.
    event MarketCreated(
        uint256 indexed marketId,
        address indexed resolver,
        address creator,
        uint64 deadline,
        bytes32 metadataDigest
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

#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("log is missing the MarketCreated signature topic")]
    WrongSignature,
    #[error("malformed MarketCreated log: {0}")]
    Malformed(#[source] SolTypeError),
}

pub const fn market_created_topic() -> B256 {
    MarketCreated::SIGNATURE_HASH
}

pub fn decode_market_created(
    topics: &[B256],
    data: &[u8],
) -> Result<MarketCreatedProjection, DecodeError> {
    if topics.first() != Some(&MarketCreated::SIGNATURE_HASH) {
        return Err(DecodeError::WrongSignature);
    }

    let event = MarketCreated::decode_raw_log_validate(topics.iter().copied(), data)
        .map_err(DecodeError::Malformed)?;

    Ok(MarketCreatedProjection {
        market_id: event.marketId,
        resolver: event.resolver,
        creator: event.creator,
        deadline: event.deadline,
        metadata_digest: event.metadataDigest,
    })
}

#[cfg(test)]
mod tests {
    use alloy::{
        primitives::{Address, B256, U256},
        sol_types::SolEvent,
    };

    use super::{DecodeError, MarketCreated, decode_market_created};

    #[test]
    fn decodes_indexed_topics_and_non_indexed_data() {
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
    fn rejects_wrong_signature_and_malformed_data() {
        let wrong_topics = [B256::repeat_byte(0xff)];
        assert!(matches!(
            decode_market_created(&wrong_topics, &[]),
            Err(DecodeError::WrongSignature)
        ));

        let malformed_topics = [MarketCreated::SIGNATURE_HASH, B256::ZERO, B256::ZERO];
        assert!(matches!(
            decode_market_created(&malformed_topics, &[0; 31]),
            Err(DecodeError::Malformed(_))
        ));
    }
}
