// Ninimma modification: independent project branding and integration.
//! Turning text into vectors for meaning-based search, locally.
//!
//! [`EmbeddingProvider`] is the replaceable part; [`local`] implements it
//! with a GGUF model run by llama.cpp in this process (CPU by default, no
//! server, no network). [`Embedder`] is the shared handle the index uses: it
//! loads the model only when needed, after checking its checksum, and frees
//! it again when idle, so smart search doesn't keep a model in memory.

// Builds without smart search keep this code (and its tests) but don't
// call all of it.
#![cfg_attr(not(feature = "smart-search"), allow(dead_code))]

#[cfg(feature = "smart-search")]
pub mod local;

use crate::models::{self, ModelSpec};
use std::fmt;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// At most this many pieces of text are embedded at once.
pub const MAX_BATCH: usize = 8;

pub trait EmbeddingProvider: Send + Sync {
    fn model_id(&self) -> &str;
    fn dimensions(&self) -> usize;
    /// One unit-length vector per text.
    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbeddingError {
    /// This build has no model runtime (built without `smart-search`).
    #[cfg_attr(feature = "smart-search", allow(dead_code))]
    NotBuiltIn,
    NotInstalled,
    Model(String),
    Failed(&'static str),
}

impl fmt::Display for EmbeddingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EmbeddingError::NotBuiltIn => f.write_str("smart search isn't included in this build"),
            EmbeddingError::NotInstalled => f.write_str("the smart search model isn't installed"),
            EmbeddingError::Model(e) => write!(f, "the smart search model can't be used: {e}"),
            EmbeddingError::Failed(e) => write!(f, "embedding failed: {e}"),
        }
    }
}

/// Whether this build can run models at all.
pub const fn is_built_in() -> bool {
    cfg!(feature = "smart-search")
}

/// What a piece of text is: a search, or document text being indexed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Query,
    Document,
}

/// The index's shared, lazily loaded model.
pub struct Embedder {
    spec: &'static ModelSpec,
    /// A model file elsewhere than the models folder (tests).
    path: Option<std::path::PathBuf>,
    threads: Mutex<i32>,
    loaded: Mutex<Option<(Box<dyn EmbeddingProvider>, Instant)>>,
    /// Searches waiting for the model. The indexer steps aside for them:
    /// the lock isn't fair, and indexing would otherwise keep winning it.
    queries_waiting: std::sync::atomic::AtomicUsize,
}

impl Embedder {
    pub fn new(spec: &'static ModelSpec) -> Self {
        Self {
            spec,
            path: None,
            threads: Mutex::new(1),
            loaded: Mutex::new(None),
            queries_waiting: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Uses the model file at `path` (still checked against `spec`).
    pub fn at(spec: &'static ModelSpec, path: std::path::PathBuf) -> Self {
        Self {
            path: Some(path),
            ..Self::new(spec)
        }
    }

    fn model_path(&self) -> Option<std::path::PathBuf> {
        let path = self.path.clone().or_else(|| models::path_of(self.spec))?;
        std::fs::metadata(&path)
            .ok()
            .filter(|m| m.is_file() && m.len() == self.spec.size)
            .map(|_| path)
    }

    pub fn spec(&self) -> &'static ModelSpec {
        self.spec
    }

    /// CPU threads for the model (the "worker threads" setting).
    pub fn set_threads(&self, threads: usize) {
        *self.threads.lock().unwrap() = threads.clamp(1, 8) as i32;
    }

    pub fn is_available(&self) -> bool {
        is_built_in() && self.model_path().is_some()
    }

    pub fn embed(&self, texts: &[String], kind: Kind) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        let prefixed: Vec<String>;
        let texts = match kind {
            Kind::Query if !self.spec.query_prefix.is_empty() => {
                prefixed = texts
                    .iter()
                    .map(|t| format!("{}{t}", self.spec.query_prefix))
                    .collect();
                &prefixed
            }
            _ => texts,
        };
        let mut out = Vec::with_capacity(texts.len());
        // The model is locked for one text at a time, so a search waiting
        // behind the indexer waits for one text (~20 ms), not a batch.
        use std::sync::atomic::Ordering;
        let is_query = kind == Kind::Query;
        if is_query {
            self.queries_waiting.fetch_add(1, Ordering::SeqCst);
        }
        let _done = QueryDone(is_query.then_some(&self.queries_waiting));
        for text in texts {
            while !is_query && self.queries_waiting.load(Ordering::SeqCst) > 0 {
                std::thread::sleep(Duration::from_millis(2));
            }
            let mut loaded = self.loaded.lock().unwrap();
            if loaded.is_none() {
                *loaded = Some((self.load()?, Instant::now()));
            }
            let (provider, used) = loaded.as_mut().expect("loaded");
            if provider.model_id() != self.spec.id || provider.dimensions() != self.spec.dimensions
            {
                return Err(EmbeddingError::Model("not the expected model".into()));
            }
            *used = Instant::now();
            out.extend(provider.embed(std::slice::from_ref(text))?);
        }
        Ok(out)
    }

    #[cfg(feature = "smart-search")]
    fn load(&self) -> Result<Box<dyn EmbeddingProvider>, EmbeddingError> {
        let path = self.model_path().ok_or(EmbeddingError::NotInstalled)?;
        models::verify(self.spec, &path).map_err(|e| EmbeddingError::Model(e.to_string()))?;
        let threads = *self.threads.lock().unwrap();
        Ok(Box::new(local::LlamaEmbedder::load(
            self.spec, &path, threads,
        )?))
    }

    #[cfg(not(feature = "smart-search"))]
    fn load(&self) -> Result<Box<dyn EmbeddingProvider>, EmbeddingError> {
        Err(EmbeddingError::NotBuiltIn)
    }

    /// Frees the model if it hasn't been used for `idle`. Returns whether
    /// a model is still loaded.
    pub fn unload_if_idle(&self, idle: Duration) -> bool {
        let mut loaded = self.loaded.lock().unwrap();
        if loaded
            .as_ref()
            .is_some_and(|(_, used)| used.elapsed() >= idle)
        {
            *loaded = None;
        }
        loaded.is_some()
    }

    pub fn unload(&self) {
        *self.loaded.lock().unwrap() = None;
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded.lock().unwrap().is_some()
    }
}

/// Marks a search as no longer waiting, however `embed` returns.
struct QueryDone<'a>(Option<&'a std::sync::atomic::AtomicUsize>);

impl Drop for QueryDone<'_> {
    fn drop(&mut self) {
        if let Some(n) = self.0 {
            n.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

/// Vectors are stored as 8-bit integers plus one scale: a quarter of the
/// size of floats, and accurate enough to rank by similarity.
pub fn quantize(v: &[f32]) -> Vec<u8> {
    let max = v
        .iter()
        .fold(0f32, |m, x| m.max(x.abs()))
        .max(f32::MIN_POSITIVE);
    let scale = max / 127.0;
    let mut out = Vec::with_capacity(4 + v.len());
    out.extend_from_slice(&scale.to_le_bytes());
    out.extend(
        v.iter()
            .map(|x| ((x / scale).round().clamp(-127.0, 127.0) as i8) as u8),
    );
    out
}

/// Cosine similarity of a unit query vector and a stored (unit) vector.
pub fn similarity(query: &[f32], stored: &[u8]) -> Option<f32> {
    let (scale, values) = stored.split_first_chunk::<4>()?;
    if values.len() != query.len() {
        return None;
    }
    let scale = f32::from_le_bytes(*scale);
    let dot: f32 = query
        .iter()
        .zip(values)
        .map(|(q, v)| q * (*v as i8) as f32)
        .sum();
    Some(dot * scale)
}

/// Scales a vector to unit length.
pub fn normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantized_vectors_keep_their_similarities() {
        let mut a: Vec<f32> = (0..384).map(|i| ((i * 7) % 13) as f32 - 6.0).collect();
        let mut b: Vec<f32> = (0..384).map(|i| ((i * 5) % 11) as f32 - 5.0).collect();
        normalize(&mut a);
        normalize(&mut b);
        let exact: f32 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
        let approx = similarity(&a, &quantize(&b)).unwrap();
        assert!((exact - approx).abs() < 0.01, "{exact} vs {approx}");
        assert!((similarity(&a, &quantize(&a)).unwrap() - 1.0).abs() < 0.01);
        // A vector of the wrong size isn't compared.
        assert_eq!(similarity(&a[..10], &quantize(&b)), None);
    }

    #[test]
    fn without_a_model_embedding_reports_why() {
        let e = Embedder::new(&models::EMBEDDING_MODEL);
        if !e.is_available() {
            assert!(e.embed(&["x".into()], Kind::Query).is_err());
            assert!(!e.is_loaded());
        }
    }
}
