use async_trait::async_trait;
use ticket_core::ports::Embedder;
use ticket_core::{Embedding, Error, Result};

/// An offline, deterministic embedder based on the feature-hashing trick.
///
/// Tokens and adjacent-token bigrams are hashed into a fixed number of buckets
/// with a signed FNV-1a hash, and the vector is L2-normalised. This captures
/// lexical overlap only (no synonyms or semantics). That makes it a good
/// default for development, tests and air-gapped deployments, and a baseline
/// to measure neural embedding models against.
///
/// FNV-1a is used instead of `std`'s hasher because its output is stable
/// across Rust releases and platforms, which stored vectors depend on.
#[derive(Debug, Clone)]
pub struct HashingEmbedder {
    dim: usize,
    model_id: String,
}

impl HashingEmbedder {
    /// Default vector width.
    pub const DEFAULT_DIM: usize = 512;

    /// Creates an embedder producing `dim`-dimensional vectors.
    pub fn new(dim: usize) -> Result<Self> {
        if dim == 0 {
            return Err(Error::Config("embedding dimension must be positive".into()));
        }
        Ok(Self {
            dim,
            model_id: format!("hashing-fnv1a-v1-{dim}"),
        })
    }

    /// Embeds synchronously. The async trait method delegates here.
    #[must_use]
    pub fn embed_text(&self, text: &str) -> Embedding {
        let mut vector = vec![0.0_f32; self.dim];
        let tokens: Vec<String> = tokenize(text).collect();
        for token in &tokens {
            self.accumulate(&mut vector, token.as_bytes(), 1.0);
        }
        for pair in tokens.windows(2) {
            let bigram = format!("{} {}", pair[0], pair[1]);
            self.accumulate(&mut vector, bigram.as_bytes(), 0.5);
        }
        Embedding::new(vector).normalized()
    }

    fn accumulate(&self, vector: &mut [f32], feature: &[u8], weight: f32) {
        let hash = fnv1a(feature);
        // The bucket comes from the low bits and the sign from the top bit.
        let bucket = (hash % self.dim as u64) as usize;
        let sign = if hash >> 63 == 0 { 1.0 } else { -1.0 };
        vector[bucket] += sign * weight;
    }
}

impl Default for HashingEmbedder {
    fn default() -> Self {
        Self {
            dim: Self::DEFAULT_DIM,
            model_id: format!("hashing-fnv1a-v1-{}", Self::DEFAULT_DIM),
        }
    }
}

#[async_trait]
impl Embedder for HashingEmbedder {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    async fn embed(&self, text: &str) -> Result<Embedding> {
        Ok(self.embed_text(text))
    }
}

const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "for", "from", "has", "have", "i",
    "in", "is", "it", "its", "my", "of", "on", "or", "our", "so", "that", "the", "this", "to",
    "was", "we", "were", "with", "you", "your",
];

fn tokenize(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .filter(|t| !STOPWORDS.contains(&t.as_str()))
}

fn fnv1a(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    bytes.iter().fold(OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(PRIME)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_text_scores_one() {
        let e = HashingEmbedder::default();
        let a = e.embed_text("Password reset email never arrived");
        let b = e.embed_text("password RESET email, never arrived!");
        assert!((a.cosine(&b) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn related_text_scores_higher_than_unrelated_text() {
        let e = HashingEmbedder::default();
        let query = e.embed_text("Password reset email not received");
        let related = e.embed_text("User never received the password reset email");
        let unrelated = e.embed_text("Invoice shows a duplicate charge for March");
        let related_score = query.cosine(&related);
        let unrelated_score = query.cosine(&unrelated);
        assert!(related_score > 0.4, "related score {related_score}");
        assert!(unrelated_score < 0.2, "unrelated score {unrelated_score}");
    }

    #[test]
    fn output_is_stable_across_instances() {
        let a = HashingEmbedder::new(64)
            .unwrap()
            .embed_text("database timeout");
        let b = HashingEmbedder::new(64)
            .unwrap()
            .embed_text("database timeout");
        assert_eq!(a, b);
        assert_eq!(a.dim(), 64);
    }

    #[test]
    fn known_fnv1a_vector() {
        // Reference value from the FNV specification test suite.
        assert_eq!(fnv1a(b"a"), 0xaf63_dc4c_8601_ec8c);
    }

    #[test]
    fn zero_dimension_is_rejected() {
        assert!(HashingEmbedder::new(0).is_err());
    }
}
