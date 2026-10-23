// Ninimma modification: independent project branding and integration.
//! Embeddings from a local GGUF model, run by llama.cpp in this process.

use super::{normalize, EmbeddingError, EmbeddingProvider, MAX_BATCH};
use crate::models::ModelSpec;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::LlamaModel;
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

/// llama.cpp's global state: set up once per process, never torn down.
pub(crate) fn backend() -> Result<&'static LlamaBackend, EmbeddingError> {
    static BACKEND: OnceLock<Option<LlamaBackend>> = OnceLock::new();
    BACKEND
        .get_or_init(|| {
            let backend = LlamaBackend::init().ok()?;
            // llama.cpp, ggml and the vision part (mtmd) log to stderr by
            // default; nothing they log is needed, and log lines could
            // include text being read. All three are switched off.
            llama_cpp_2::send_logs_to_tracing(
                llama_cpp_2::LogOptions::default().with_logs_enabled(false),
            );
            Some(backend)
        })
        .as_ref()
        .ok_or(EmbeddingError::Failed("llama.cpp couldn't start"))
}

pub struct LlamaEmbedder {
    /// One context, reused for every text (creating and freeing ~30 MB of
    /// buffers per text fragmented memory). It must be dropped before the
    /// model, so it's declared first: fields drop in order.
    context: Mutex<Option<LlamaContext<'static>>>,
    id: &'static str,
    model: LlamaModel,
    dimensions: usize,
    /// Tokens per text the model was trained on; longer texts are cut.
    max_tokens: usize,
    threads: i32,
}

impl LlamaEmbedder {
    /// Loads a model file that has already passed its checksum.
    pub fn load(
        spec: &'static ModelSpec,
        path: &Path,
        threads: i32,
    ) -> Result<Self, EmbeddingError> {
        let backend = backend()?;
        // CPU only unless built with a GPU feature (then offload all layers).
        let params = if cfg!(any(feature = "gpu-vulkan", feature = "gpu-metal")) {
            LlamaModelParams::default().with_n_gpu_layers(1000)
        } else {
            LlamaModelParams::default().with_n_gpu_layers(0)
        };
        let model = LlamaModel::load_from_file(backend, path, &params)
            .map_err(|e| EmbeddingError::Model(e.to_string()))?;
        let dimensions = model.n_embd().max(0) as usize;
        if dimensions != spec.dimensions {
            return Err(EmbeddingError::Model(format!(
                "expected {} dimensions, the file has {dimensions}",
                spec.dimensions
            )));
        }
        let max_tokens = (model.n_ctx_train() as usize).clamp(16, 8192);
        Ok(Self {
            context: Mutex::new(None),
            id: spec.id,
            model,
            dimensions,
            max_tokens,
            threads,
        })
    }
}

impl EmbeddingProvider for LlamaEmbedder {
    fn model_id(&self) -> &str {
        self.id
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        if texts.len() > MAX_BATCH {
            return Err(EmbeddingError::Failed("too many texts at once"));
        }
        let backend = backend()?;
        let vocab = self.model.vocab();
        let tokens: Vec<_> = texts
            .iter()
            .map(|t| {
                let mut tokens = vocab.tokenize(t.as_bytes(), true, false);
                tokens.truncate(self.max_tokens);
                tokens
            })
            .collect();
        // One text per pass: attention memory grows with the square of the
        // tokens in a pass, so batching texts together multiplies memory
        // (8 texts of 256 tokens needed ~500 MB; one at a time, ~30 MB).
        let mut context = self.context.lock().unwrap();
        if context.is_none() {
            let size = self.max_tokens as u32;
            let params = LlamaContextParams::default()
                .with_embeddings(true)
                .with_n_ctx(NonZeroU32::new(size))
                .with_n_batch(size)
                .with_n_ubatch(size)
                .with_n_seq_max(1)
                .with_n_threads(self.threads)
                .with_n_threads_batch(self.threads);
            let ctx = self
                .model
                .new_context(backend, params)
                .map_err(|_| EmbeddingError::Failed("couldn't set up the model"))?;
            // SAFETY: the context points at llama.cpp's model, which lives
            // until `self.model` is dropped; `context` is declared before
            // `model`, so it is always dropped first, and it never leaves
            // this struct.
            *context = Some(unsafe {
                std::mem::transmute::<LlamaContext<'_>, LlamaContext<'static>>(ctx)
            });
        }
        let ctx = context.as_mut().expect("created above");
        let longest = self.max_tokens;
        let mut batch = LlamaBatch::new(longest, 1);
        let mut out = Vec::with_capacity(texts.len());
        for t in &tokens {
            batch.clear();
            if t.is_empty() {
                out.push(vec![0.0; self.dimensions]);
                continue;
            }
            batch
                .add_sequence(t, 0, false)
                .map_err(|_| EmbeddingError::Failed("text too long"))?;
            ctx.clear_kv_cache();
            // Encoder models (BERT-style) encode; others decode.
            if ctx.encode(&mut batch).is_err() {
                ctx.clear_kv_cache();
                ctx.decode(&mut batch)
                    .map_err(|_| EmbeddingError::Failed("the model couldn't process the text"))?;
            }
            let mut v = ctx
                .embeddings_seq_ith(0)
                .map_err(|_| EmbeddingError::Failed("no embedding produced"))?
                .to_vec();
            normalize(&mut v);
            out.push(v);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{quantize, similarity, Embedder, Kind};
    use crate::models::EMBEDDING_MODEL;

    /// Needs the model, so it's opt-in: `NINIMMA_TEST_MODEL=<path to
    /// the .gguf> cargo test -- --ignored meaning` (or install it from the
    /// app first).
    #[test]
    #[ignore]
    fn meaning_based_similarity_with_the_real_model() {
        let e = match std::env::var_os("NINIMMA_TEST_MODEL") {
            Some(path) => Embedder::at(&EMBEDDING_MODEL, path.into()),
            None => Embedder::new(&EMBEDDING_MODEL),
        };
        assert!(
            e.is_available(),
            "install the model, or set NINIMMA_TEST_MODEL"
        );
        e.set_threads(2);
        let query = &e
            .embed(&["receipts from my Arizona trip".into()], Kind::Query)
            .unwrap()[0];
        let docs = e
            .embed(
                &[
                    "Red Rock Lodge, Sedona AZ. RECEIPT. Total $412.50".into(),
                    "Invoice 2231, Grand Canyon tours, amount due $89".into(),
                    "Quarterly forecast for the zebra project".into(),
                ],
                Kind::Document,
            )
            .unwrap();
        let score = |d: &Vec<f32>| similarity(query, &quantize(d)).unwrap();
        assert!(score(&docs[0]) > score(&docs[2]) + 0.1);
        assert!(
            score(&docs[1]) > score(&docs[2]) + 0.1,
            "an invoice is close to a receipt"
        );
        e.unload();
        assert!(!e.is_loaded());
    }
}
