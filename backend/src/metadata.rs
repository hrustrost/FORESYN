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
    /// The serialization order is fixed to ensure consistent hashing:
    /// 1. question
    /// 2. description
    /// 3. resolution_criteria
    /// 4. category
    /// 5. source_url (nullable)
    pub fn to_canonical_json(&self) -> Result<Vec<u8>, MetadataError> {
        // Use a structured approach to ensure field order
        let canonical = serde_json::json!({
            "question": self.question,
            "description": self.description,
            "resolution_criteria": self.resolution_criteria,
            "category": self.category,
            "source_url": self.source_url,
        });

        serde_json::to_vec(&canonical)
            .map_err(|e| MetadataError::Serialization(e.to_string()))
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
        assert_eq!(parsed["question"], "Will ETH be above $4,000 on August 22, 2026?");
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
