// Ninimma modification: independent project branding and integration.
//! OCR with IBM granite-docling through llama.cpp's multimodal support.

use super::{OcrError, TextReader};
use image::RgbImage;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{LlamaChatMessage, LlamaModel};
use llama_cpp_2::mtmd::{
    mtmd_default_marker, MtmdBitmap, MtmdContext, MtmdContextParams, MtmdInputText,
};
use llama_cpp_2::sampling::LlamaSampler;
use std::num::NonZeroU32;
use std::path::Path;

/// The model's own instruction for reading a page.
const PROMPT: &str = "Convert this page to docling.";
/// Enough for a long receipt or a page of text; generation stops early at
/// the end of the page.
const MAX_TOKENS: usize = 1500;
const CONTEXT: u32 = 4096;
/// Tokens processed per step while reading the image. (Memory is set by
/// the model files and the vision encoder: 256 made no difference.)
const BATCH: u32 = 1024;

pub struct GraniteOcr {
    /// Dropped before the model (fields drop in order); see `load`.
    context: Option<LlamaContext<'static>>,
    vision: MtmdContext,
    model: LlamaModel,
}

impl GraniteOcr {
    /// Loads model files that have already passed their checksums.
    pub fn load(model_path: &Path, projector: &Path, threads: i32) -> Result<Self, OcrError> {
        let backend =
            crate::embedding::local::backend().map_err(|e| OcrError::Model(e.to_string()))?;
        let params = if cfg!(any(feature = "gpu-vulkan", feature = "gpu-metal")) {
            LlamaModelParams::default().with_n_gpu_layers(1000)
        } else {
            LlamaModelParams::default().with_n_gpu_layers(0)
        };
        let model = LlamaModel::load_from_file(backend, model_path, &params)
            .map_err(|e| OcrError::Model(e.to_string()))?;
        let vparams = MtmdContextParams {
            use_gpu: cfg!(any(feature = "gpu-vulkan", feature = "gpu-metal")),
            n_threads: threads,
            print_timings: false,
            ..MtmdContextParams::default()
        };
        let projector = projector
            .to_str()
            .ok_or(OcrError::Failed("the model folder's path isn't valid text"))?;
        let vision = MtmdContext::init_from_file(projector, &model, &vparams)
            .map_err(|e| OcrError::Model(e.to_string()))?;
        if !vision.support_vision() {
            return Err(OcrError::Model(
                "the vision part doesn't support images".into(),
            ));
        }
        let cparams = LlamaContextParams::default()
            .with_n_ctx(NonZeroU32::new(CONTEXT))
            .with_n_batch(BATCH)
            .with_n_ubatch(BATCH)
            .with_n_threads(threads)
            .with_n_threads_batch(threads);
        let context = model
            .new_context(backend, cparams)
            .map_err(|_| OcrError::Failed("couldn't set up the model"))?;
        // SAFETY: the context points at llama.cpp's model, which lives until
        // `self.model` is dropped; `context` is declared first, so it's
        // dropped before `vision` and `model`, and it never leaves `self`.
        let context =
            unsafe { std::mem::transmute::<LlamaContext<'_>, LlamaContext<'static>>(context) };
        Ok(Self {
            context: Some(context),
            vision,
            model,
        })
    }
}

impl TextReader for GraniteOcr {
    fn read(&mut self, image: &RgbImage) -> Result<String, OcrError> {
        let ctx = self
            .context
            .as_mut()
            .ok_or(OcrError::Failed("no model context"))?;
        ctx.clear_kv_cache();
        let bitmap = MtmdBitmap::from_image_data(image.width(), image.height(), image.as_raw())
            .map_err(|_| OcrError::Failed("the image couldn't be prepared"))?;
        let template = self
            .model
            .chat_template(None)
            .map_err(|_| OcrError::Model("no chat template".into()))?;
        let message =
            LlamaChatMessage::new("user".into(), format!("{}{PROMPT}", mtmd_default_marker()))
                .map_err(|_| OcrError::Failed("bad prompt"))?;
        let prompt = self
            .model
            .apply_chat_template(&template, &[message], true)
            .map_err(|_| OcrError::Model("the chat template failed".into()))?;
        let chunks = self
            .vision
            .tokenize(
                MtmdInputText {
                    text: prompt,
                    add_special: true,
                    parse_special: true,
                },
                &[&bitmap],
            )
            .map_err(|_| OcrError::Failed("the image couldn't be tokenized"))?;
        let start = chunks
            .eval_chunks(&mut self.vision, ctx, 0, 0, BATCH as i32, true)
            .map_err(|_| OcrError::Failed("the model couldn't read the image"))?;
        let vocab = self.model.vocab();
        let mut sampler = LlamaSampler::greedy();
        let mut batch = LlamaBatch::new(1, 1);
        let mut bytes = Vec::new();
        let end = (start + MAX_TOKENS as i32).min(CONTEXT as i32 - 1);
        for position in start..end {
            let token = sampler.sample(ctx, -1);
            sampler.accept(token);
            if vocab.is_eog(token) {
                break;
            }
            bytes.extend(vocab.token_to_piece(token, false, None));
            batch.clear();
            batch
                .add(token, position, &[0], true)
                .map_err(|_| OcrError::Failed("batch full"))?;
            ctx.decode(&mut batch)
                .map_err(|_| OcrError::Failed("the model stopped"))?;
        }
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::super::Ocr;

    /// Needs the OCR model files, so it's opt-in:
    /// `NINIMMA_TEST_OCR=<dir with both .gguf files> cargo test --release -- --ignored ocr_reads`
    #[test]
    #[ignore]
    fn ocr_reads_a_receipt() {
        let dir = std::path::PathBuf::from(
            std::env::var_os("NINIMMA_TEST_OCR").expect("set NINIMMA_TEST_OCR"),
        );
        let ocr = Ocr::at(
            dir.join(crate::models::OCR_MODEL.file),
            dir.join(crate::models::OCR_PROJECTOR.file),
        );
        ocr.set_threads(2);
        let image = super::super::tests_support::receipt_image();
        let text = ocr.read(&image).unwrap();
        for words in ["RED ROCK LODGE", "Sedona, AZ", "$412.50", "Thank you"] {
            assert!(text.contains(words), "{words:?} missing from {text:?}");
        }
        ocr.unload();
    }
}
