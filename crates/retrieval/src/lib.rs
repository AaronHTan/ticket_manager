//! Embedding providers for retrieval-augmented triage.
//!
//! Every provider implements [`ticket_core::ports::Embedder`]. The engine
//! never depends on a concrete provider, so moving from the offline
//! [`HashingEmbedder`] to a neural model is a configuration change. Heavy
//! dependencies (HTTP clients, ONNX runtimes) belong here behind cargo
//! features, not in the engine.

mod hashing;

pub use hashing::HashingEmbedder;
