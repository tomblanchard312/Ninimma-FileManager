// Ninimma modification: independent project branding and integration.
//! The query model, run by llama.cpp with its output held to `GRAMMAR`.
//!
//! Tokens are chosen without llama.cpp's combined "sample" call: the
//! grammar is applied to the candidates, the best allowed token is picked
//! here, then accepted. Accepting a token the grammar allows can't fail, so
//! no C++ exception (which would abort the app) can reach Rust.

use super::{Interpreter, GRAMMAR};
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{LlamaChatMessage, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use std::num::NonZeroU32;
use std::path::Path;

const INSTRUCTIONS: &str = "You pick the words that identify the files someone is searching for. \
Answer with JSON: words = the important words from the search (names, things, topics), copied \
exactly; never add words that aren't in the search; leave out dates and filler; place = a place \
named in the search, or null; kind = image, video or audio if the search asks for photos, videos \
or music, else null.
Examples:
Q: Show all receipts from Arizona trip on December
A: {\"words\":[\"receipts\",\"trip\"],\"place\":\"Arizona\",\"kind\":null}
Q: photos of the kids at the beach last summer
A: {\"words\":[\"kids\",\"beach\"],\"place\":null,\"kind\":\"image\"}
Q: where is the letter from the IRS
A: {\"words\":[\"letter\",\"IRS\"],\"place\":null,\"kind\":null}";

const CONTEXT: u32 = 1024;
const MAX_TOKENS: usize = 80;

pub struct QueryModel {
    /// Dropped before the model (fields drop in order).
    context: Option<LlamaContext<'static>>,
    model: LlamaModel,
}

impl QueryModel {
    /// Loads a model file that has already passed its checksum.
    pub fn load(path: &Path) -> Result<Self, ()> {
        let backend = crate::embedding::local::backend().map_err(|_| ())?;
        let model = LlamaModel::load_from_file(backend, path, &LlamaModelParams::default())
            .map_err(|_| ())?;
        // The grammar must compile; checked once here so a mistake fails
        // loudly in tests rather than quietly per search.
        LlamaSampler::grammar(&model, GRAMMAR, "root").map_err(|_| ())?;
        let params = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(CONTEXT))
            .with_n_batch(CONTEXT)
            .with_n_ubatch(CONTEXT)
            .with_n_threads(2)
            .with_n_threads_batch(2);
        let context = model.new_context(backend, params).map_err(|_| ())?;
        // SAFETY: the context points at llama.cpp's model, which lives until
        // `self.model` is dropped; `context` is declared first, so it's
        // dropped first, and it never leaves `self`.
        let context =
            unsafe { std::mem::transmute::<LlamaContext<'_>, LlamaContext<'static>>(context) };
        Ok(Self {
            context: Some(context),
            model,
        })
    }
}

impl Interpreter for QueryModel {
    fn interpret(&mut self, question: &str, cancelled: &dyn Fn() -> bool) -> Option<String> {
        let ctx = self.context.as_mut()?;
        let template = self.model.chat_template(None).ok()?;
        let messages = [
            LlamaChatMessage::new("system".into(), INSTRUCTIONS.into()).ok()?,
            LlamaChatMessage::new("user".into(), format!("Q: {question}")).ok()?,
        ];
        let prompt = self
            .model
            .apply_chat_template(&template, &messages, true)
            .ok()?
            + "A: ";
        let tokens = self.model.vocab().tokenize(prompt.as_bytes(), false, true);
        if tokens.len() + MAX_TOKENS >= CONTEXT as usize {
            return None;
        }
        ctx.clear_kv_cache();
        let mut batch = LlamaBatch::new(CONTEXT as usize, 1);
        batch.add_sequence(&tokens, 0, false).ok()?;
        ctx.decode(&mut batch).ok()?;
        let mut grammar = LlamaSampler::grammar(&self.model, GRAMMAR, "root").ok()?;
        let vocab = self.model.vocab();
        let mut out = Vec::new();
        let mut depth = 0i32;
        for position in tokens.len()..tokens.len() + MAX_TOKENS {
            if cancelled() {
                return None;
            }
            let mut candidates = ctx.token_data_array_ith(-1);
            candidates.apply_sampler(&mut grammar);
            let best = candidates
                .data
                .iter()
                .filter(|t| t.logit().is_finite())
                .max_by(|a, b| a.logit().total_cmp(&b.logit()))?
                .id();
            if vocab.is_eog(best) {
                break;
            }
            grammar.accept(best);
            let piece = vocab.token_to_piece(best, false, None);
            for b in &piece {
                match b {
                    b'{' => depth += 1,
                    b'}' => depth -= 1,
                    _ => {}
                }
            }
            out.extend(piece);
            if depth == 0 && out.contains(&b'}') {
                break; // the object is complete
            }
            batch.clear();
            batch.add(best, position as i32, &[0], true).ok()?;
            ctx.decode(&mut batch).ok()?;
        }
        String::from_utf8(out).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::super::Understander;

    /// Opt-in: `NINIMMA_TEST_QUERY_MODEL=<qwen .gguf> cargo test
    /// --release -- --ignored understands`.
    #[test]
    #[ignore]
    fn understands_sentences_without_inventing_words() {
        let path =
            std::env::var_os("NINIMMA_TEST_QUERY_MODEL").expect("set NINIMMA_TEST_QUERY_MODEL");
        let u = Understander::at(path.into());
        for question in [
            "Show all receipts from Arizona trip on December",
            "the contract I signed with Acme in March 2025",
            "pictures from grandma's birthday party last month",
            "videos from our trip to Paris",
        ] {
            let started = std::time::Instant::now();
            let answer = u.understand(question, &|| false);
            println!(
                "{:>5} ms  {question}\n        {answer:?}",
                started.elapsed().as_millis()
            );
            let answer = answer.expect("an answer");
            // Every word came from the question (the check enforces it).
            let lower = question.to_lowercase();
            assert!(answer.words.iter().all(|w| lower.contains(w.as_str())));
        }
        // Asking again is answered from the cache.
        let started = std::time::Instant::now();
        u.understand("videos from our trip to Paris", &|| false);
        assert!(started.elapsed().as_millis() < 5);
    }
}
