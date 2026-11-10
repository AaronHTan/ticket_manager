use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// A dense vector for some text, produced by an
/// [`Embedder`](crate::ports::Embedder).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Embedding(Vec<f32>);

impl Embedding {
    /// Wraps raw vector components.
    #[must_use]
    pub fn new(values: Vec<f32>) -> Self {
        Self(values)
    }

    /// Number of dimensions.
    #[must_use]
    pub fn dim(&self) -> usize {
        self.0.len()
    }

    /// Raw components.
    #[must_use]
    pub fn as_slice(&self) -> &[f32] {
        &self.0
    }

    /// Euclidean length of the vector.
    #[must_use]
    pub fn l2_norm(&self) -> f32 {
        self.0.iter().map(|v| v * v).sum::<f32>().sqrt()
    }

    /// Returns a unit-length copy. A zero vector is returned unchanged.
    #[must_use]
    pub fn normalized(mut self) -> Self {
        let norm = self.l2_norm();
        if norm > f32::EPSILON {
            self.0.iter_mut().for_each(|v| *v /= norm);
        }
        self
    }

    /// Cosine similarity in `[-1.0, 1.0]`.
    ///
    /// Returns `0.0` when the dimensions differ or either vector is zero, so a
    /// mismatched embedding model can never produce a false-positive match.
    #[must_use]
    pub fn cosine(&self, other: &Self) -> f32 {
        if self.dim() != other.dim() || self.0.is_empty() {
            return 0.0;
        }
        let dot: f32 = self.0.iter().zip(&other.0).map(|(a, b)| a * b).sum();
        let denom = self.l2_norm() * other.l2_norm();
        if denom <= f32::EPSILON {
            0.0
        } else {
            (dot / denom).clamp(-1.0, 1.0)
        }
    }

    /// Serialises to little-endian `f32` bytes, for BLOB storage.
    #[must_use]
    pub fn to_le_bytes(&self) -> Vec<u8> {
        self.0.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    /// Deserialises from little-endian `f32` bytes.
    pub fn from_le_bytes(bytes: &[u8]) -> Result<Self> {
        let (chunks, remainder) = bytes.as_chunks::<4>();
        if !remainder.is_empty() {
            return Err(Error::Validation(format!(
                "embedding byte length {} is not a multiple of 4",
                bytes.len()
            )));
        }
        Ok(Self(
            chunks.iter().copied().map(f32::from_le_bytes).collect(),
        ))
    }
}

/// Keeps the `k` highest-scoring items, sorted by descending score.
///
/// Non-finite scores are discarded.
pub fn top_k<T>(items: impl IntoIterator<Item = (T, f32)>, k: usize) -> Vec<(T, f32)> {
    let mut scored: Vec<_> = items.into_iter().filter(|(_, s)| s.is_finite()).collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    scored.truncate(k);
    scored
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_of_identical_and_orthogonal_vectors() {
        let a = Embedding::new(vec![1.0, 0.0, 0.0]);
        let b = Embedding::new(vec![0.0, 2.0, 0.0]);
        assert!((a.cosine(&a) - 1.0).abs() < 1e-6);
        assert!(a.cosine(&b).abs() < 1e-6);
    }

    #[test]
    fn cosine_is_zero_for_mismatched_or_zero_vectors() {
        let a = Embedding::new(vec![1.0, 0.0]);
        assert_eq!(a.cosine(&Embedding::new(vec![1.0, 0.0, 0.0])), 0.0);
        assert_eq!(a.cosine(&Embedding::new(vec![0.0, 0.0])), 0.0);
    }

    #[test]
    fn normalized_has_unit_length() {
        let e = Embedding::new(vec![3.0, 4.0]).normalized();
        assert!((e.l2_norm() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn bytes_round_trip() {
        let e = Embedding::new(vec![0.25, -1.5, 3.0]);
        assert_eq!(Embedding::from_le_bytes(&e.to_le_bytes()).unwrap(), e);
        assert!(Embedding::from_le_bytes(&[0, 1, 2]).is_err());
    }

    #[test]
    fn top_k_sorts_descending_and_truncates() {
        let ranked = top_k([("a", 0.2), ("b", 0.9), ("c", f32::NAN), ("d", 0.5)], 2);
        assert_eq!(ranked, vec![("b", 0.9), ("d", 0.5)]);
    }
}
