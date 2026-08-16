use alloy::primitives::B256;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Represents market metadata that is stored off-chain but committed on-chain via digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketMetadata {
    pub question: String,
    pub description: String,
    pub resolution_criteria: String,
    pub category: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
}

#[derive(Debug, Error)]
pub enum MetadataError {
    #[error("metadata serialization failed: {0}")]
    Serialization(String),
    #[error("metadata hash mismatch: expected {expected}, got {actual}")]
    HashMismatch { expected: String, actual: String },
}

impl MarketMetadata {
    /// Serialize metadata to canonical JSON format for deterministic hashing.
    ///
    /// The field order is fixed and part of the digest contract:
    /// 1. question
    /// 2. description
    /// 3. resolution_criteria
    /// 4. category
    /// 5. source_url (null when absent)
    ///
    /// The object is built by explicit string concatenation rather than by
    /// serializing a `serde_json::Map`. A `Map` orders its keys according to
    /// whether serde_json's `preserve_order` feature is enabled anywhere in the
    /// dependency graph, so relying on it would let an unrelated dependency
    /// silently change every digest. Only the individual values go through
    /// serde_json, purely for correct JSON string escaping.
    ///
    /// Changing the field order, the field names, or the `source_url` null
    /// representation invalidates every digest already committed on-chain.
    pub fn to_canonical_json(&self) -> Result<Vec<u8>, MetadataError> {
        fn encode(value: &str) -> Result<String, MetadataError> {
            serde_json::to_string(value).map_err(|e| MetadataError::Serialization(e.to_string()))
        }

        let source_url = match self.source_url.as_deref() {
            Some(url) => encode(url)?,
            None => "null".to_string(),
        };

        let canonical = format!(
            concat!(
                "{{",
                "\"question\":{},",
                "\"description\":{},",
                "\"resolution_criteria\":{},",
                "\"category\":{},",
                "\"source_url\":{}",
                "}}"
            ),
            encode(&self.question)?,
            encode(&self.description)?,
            encode(&self.resolution_criteria)?,
            encode(&self.category)?,
            source_url,
        );

        Ok(canonical.into_bytes())
    }

    /// Compute keccak256 hash of canonical metadata JSON.
    ///
    /// This must match the on-chain metadataDigest that was stored when the market was created.
    pub fn compute_digest(&self) -> Result<B256, MetadataError> {
        use alloy::primitives::keccak256;
        let canonical = self.to_canonical_json()?;
        Ok(keccak256(&canonical))
    }

    /// Verify that the given digest matches the hash of this metadata.
    pub fn verify_digest(&self, expected: B256) -> Result<bool, MetadataError> {
        let actual = self.compute_digest()?;
        Ok(actual == expected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact metadata intended for Base Sepolia Market #2.
    fn market_2_metadata() -> MarketMetadata {
        MarketMetadata {
            question: "Will ETH be above $4,000 on August 22, 2026?".to_string(),
            description: "A Base Sepolia demonstration prediction market for FORESYN.".to_string(),
            resolution_criteria: "Resolves YES if the ETH/USD reference price is strictly above 4000 USD at the market deadline. Otherwise resolves NO.".to_string(),
            category: "Crypto".to_string(),
            source_url: None,
        }
    }

    /// Pins the byte-for-byte canonical form. This is the digest contract: if
    /// this test fails, every digest already committed on-chain is invalidated.
    #[test]
    fn canonical_json_bytes_are_pinned() {
        let canonical = market_2_metadata().to_canonical_json().unwrap();

        assert_eq!(
            String::from_utf8(canonical).unwrap(),
            concat!(
                r#"{"question":"Will ETH be above $4,000 on August 22, 2026?","#,
                r#""description":"A Base Sepolia demonstration prediction market for FORESYN.","#,
                r#""resolution_criteria":"Resolves YES if the ETH/USD reference price is strictly"#,
                r#" above 4000 USD at the market deadline. Otherwise resolves NO.","#,
                r#""category":"Crypto","#,
                r#""source_url":null}"#,
            ),
        );
    }

    /// Pins the digest that will be committed on-chain for Market #2, so the
    /// value quoted in the create-market command can never drift silently.
    #[test]
    fn market_2_digest_is_pinned() {
        let digest = market_2_metadata().compute_digest().unwrap();

        assert_eq!(
            format!("{digest:?}"),
            "0xca3422b0b137aacb93c63a5cd26a8edccaaa30785067afb912c72ddda02a1659",
        );
    }

    /// The canonical form must not depend on serde_json's map key ordering.
    #[test]
    fn canonical_json_field_order_is_explicit_not_alphabetical() {
        let canonical = market_2_metadata().to_canonical_json().unwrap();
        let text = String::from_utf8(canonical).unwrap();

        let question = text.find(r#""question""#).unwrap();
        let description = text.find(r#""description""#).unwrap();
        let criteria = text.find(r#""resolution_criteria""#).unwrap();
        let category = text.find(r#""category""#).unwrap();
        let source_url = text.find(r#""source_url""#).unwrap();

        assert!(question < description);
        assert!(description < criteria);
        assert!(criteria < category);
        assert!(category < source_url);
    }

    /// `source_url` must always be present as an explicit null, never omitted,
    /// otherwise a market with no source would hash differently.
    #[test]
    fn absent_source_url_is_an_explicit_null() {
        let mut metadata = market_2_metadata();
        let without = metadata.to_canonical_json().unwrap();
        assert!(
            String::from_utf8(without)
                .unwrap()
                .contains(r#""source_url":null"#)
        );

        metadata.source_url = Some("https://example.com".to_string());
        let with = metadata.to_canonical_json().unwrap();
        assert!(
            String::from_utf8(with)
                .unwrap()
                .contains(r#""source_url":"https://example.com""#)
        );
    }

    /// Values containing JSON metacharacters must be escaped, not injected raw.
    #[test]
    fn values_with_json_metacharacters_are_escaped() {
        let metadata = MarketMetadata {
            question: r#"Is "x" > 1?"#.to_string(),
            description: "line\nbreak".to_string(),
            resolution_criteria: r"back\slash".to_string(),
            category: "Crypto".to_string(),
            source_url: None,
        };

        let canonical = metadata.to_canonical_json().unwrap();
        let text = String::from_utf8(canonical).unwrap();

        // Must still parse as JSON and round-trip the original values.
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["question"], r#"Is "x" > 1?"#);
        assert_eq!(parsed["description"], "line\nbreak");
        assert_eq!(parsed["resolution_criteria"], r"back\slash");
    }

    #[test]
    fn test_metadata_canonical_json() {
        let metadata = MarketMetadata {
            question: "Will ETH be above $4,000 on August 22, 2026?".to_string(),
            description: "A Base Sepolia demonstration prediction market for FORESYN.".to_string(),
            resolution_criteria: "Resolves YES if the ETH/USD reference price is strictly above 4000 USD at the market deadline. Otherwise resolves NO.".to_string(),
            category: "Crypto".to_string(),
            source_url: None,
        };

        let canonical = metadata.to_canonical_json().unwrap();
        let canonical_str = String::from_utf8(canonical).unwrap();

        // Verify it's valid JSON and contains all fields
        assert!(canonical_str.contains("Will ETH be above $4,000"));
        assert!(canonical_str.contains("Crypto"));
        assert!(canonical_str.contains("source_url"));

        // Parse back to verify structure
        let parsed: serde_json::Value = serde_json::from_str(&canonical_str).unwrap();
        assert_eq!(
            parsed["question"],
            "Will ETH be above $4,000 on August 22, 2026?"
        );
        assert_eq!(parsed["category"], "Crypto");
    }

    #[test]
    fn test_same_metadata_same_digest() {
        let metadata1 = MarketMetadata {
            question: "Test question?".to_string(),
            description: "Test description".to_string(),
            resolution_criteria: "Test criteria".to_string(),
            category: "Test".to_string(),
            source_url: None,
        };

        let metadata2 = MarketMetadata {
            question: "Test question?".to_string(),
            description: "Test description".to_string(),
            resolution_criteria: "Test criteria".to_string(),
            category: "Test".to_string(),
            source_url: None,
        };

        let digest1 = metadata1.compute_digest().unwrap();
        let digest2 = metadata2.compute_digest().unwrap();

        assert_eq!(digest1, digest2);
    }

    #[test]
    fn test_different_metadata_different_digest() {
        let metadata1 = MarketMetadata {
            question: "Question A?".to_string(),
            description: "Description".to_string(),
            resolution_criteria: "Criteria".to_string(),
            category: "Category".to_string(),
            source_url: None,
        };

        let metadata2 = MarketMetadata {
            question: "Question B?".to_string(),
            description: "Description".to_string(),
            resolution_criteria: "Criteria".to_string(),
            category: "Category".to_string(),
            source_url: None,
        };

        let digest1 = metadata1.compute_digest().unwrap();
        let digest2 = metadata2.compute_digest().unwrap();

        assert_ne!(digest1, digest2);
    }

    #[test]
    fn test_verify_digest() {
        let metadata = MarketMetadata {
            question: "Test?".to_string(),
            description: "Desc".to_string(),
            resolution_criteria: "Criteria".to_string(),
            category: "Cat".to_string(),
            source_url: None,
        };

        let correct_digest = metadata.compute_digest().unwrap();
        assert!(metadata.verify_digest(correct_digest).unwrap());

        // Verify with wrong digest fails
        let wrong_digest = B256::default();
        assert!(!metadata.verify_digest(wrong_digest).unwrap());
    }
}
