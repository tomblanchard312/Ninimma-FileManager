// Ninimma modification: independent project branding and integration.
//! CLIP ViT-B/32 through candle (pure Rust): the weights are a safetensors
//! file, checked against its pinned SHA-256 before it's opened.

use super::{PictureError, PictureModel, IMAGE_SIDE};
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::clip::{ClipConfig, ClipModel};
use image::RgbImage;
use std::path::Path;

/// CLIP's text window.
const MAX_TOKENS: usize = 77;
/// CLIP's pixel normalisation (from its training).
const MEAN: [f32; 3] = [0.481_454_66, 0.457_827_5, 0.408_210_73];
const STD: [f32; 3] = [0.268_629_54, 0.261_302_6, 0.275_777_1];

pub struct Clip {
    model: ClipModel,
    tokenizer: tokenizers::Tokenizer,
    end: u32,
    /// The model's own threads, so a background pass uses only its share.
    pool: rayon::ThreadPool,
}

fn model_error(e: impl std::fmt::Display) -> PictureError {
    PictureError::Model(e.to_string())
}

impl Clip {
    pub fn load(weights: &Path, tokenizer: &Path, threads: usize) -> Result<Self, PictureError> {
        let config = ClipConfig::vit_base_patch32();
        // SAFETY: the file was just checked against its pinned checksum and
        // lives in our own models folder; it isn't changed while mapped.
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[weights], DType::F32, &Device::Cpu)
                .map_err(model_error)?
        };
        let model = ClipModel::new(vb, &config).map_err(model_error)?;
        let tokenizer = tokenizers::Tokenizer::from_file(tokenizer).map_err(model_error)?;
        let end = tokenizer
            .token_to_id("<|endoftext|>")
            .ok_or_else(|| model_error("the tokenizer has no end token"))?;
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads.max(1))
            .thread_name(|i| format!("pictures-{i}"))
            .build()
            .map_err(model_error)?;
        Ok(Self {
            model,
            tokenizer,
            end,
            pool,
        })
    }
}

fn pixels(images: &[RgbImage]) -> candle_core::Result<Tensor> {
    let side = IMAGE_SIDE as usize;
    let mean = Tensor::new(&MEAN, &Device::Cpu)?.reshape((3, 1, 1))?;
    let std = Tensor::new(&STD, &Device::Cpu)?.reshape((3, 1, 1))?;
    let each = images
        .iter()
        .map(|img| {
            let data = Tensor::from_vec(img.as_raw().clone(), (side, side, 3), &Device::Cpu)?
                .permute((2, 0, 1))?
                .to_dtype(DType::F32)?;
            (data / 255.0)?.broadcast_sub(&mean)?.broadcast_div(&std)
        })
        .collect::<candle_core::Result<Vec<_>>>()?;
    Tensor::stack(&each, 0)
}

impl PictureModel for Clip {
    fn pictures(&mut self, images: &[RgbImage]) -> Result<Vec<Vec<f32>>, PictureError> {
        if images.is_empty() {
            return Ok(Vec::new());
        }
        if images
            .iter()
            .any(|i| i.width() != IMAGE_SIDE || i.height() != IMAGE_SIDE)
        {
            return Err(model_error("pictures must be prepared first"));
        }
        let model = &self.model;
        self.pool
            .install(|| {
                let features = model.get_image_features(&pixels(images)?)?;
                features.to_vec2::<f32>()
            })
            .map_err(model_error)
    }

    fn texts(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, PictureError> {
        let mut ids = Vec::with_capacity(texts.len());
        for text in texts {
            let mut t = self
                .tokenizer
                .encode(text.as_str(), true)
                .map_err(model_error)?
                .get_ids()
                .to_vec();
            t.truncate(MAX_TOKENS);
            ids.push(t);
        }
        // One length for the batch, padded with the end token (CLIP reads
        // the text's meaning at its first end token).
        let len = ids.iter().map(Vec::len).max().unwrap_or(1).max(1);
        let rows = ids
            .into_iter()
            .map(|mut t| {
                t.resize(len, self.end);
                Tensor::new(t.as_slice(), &Device::Cpu)
            })
            .collect::<candle_core::Result<Vec<_>>>()
            .map_err(model_error)?;
        let model = &self.model;
        self.pool
            .install(|| {
                let features = model.get_text_features(&Tensor::stack(&rows, 0)?)?;
                features.to_vec2::<f32>()
            })
            .map_err(model_error)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{score, Pictures, MATCH_FLOOR};
    use std::collections::HashSet;
    use std::path::PathBuf;

    /// Needs the model and a folder of labelled photos, so it's opt-in:
    /// `NINIMMA_TEST_PICTURES=<dir with both model files>
    /// NINIMMA_TEST_PHOTOS=<dir of the photos named below>
    /// cargo test --release -- --ignored pictures_are_found`
    ///
    /// The 36 photos used to set `MATCH_FLOOR` came from Wikimedia
    /// Commons (not kept in the repository); any photos work if the labels
    /// below are changed to match them.
    #[test]
    #[ignore]
    fn pictures_are_found_by_what_they_show() {
        let models = PathBuf::from(
            std::env::var_os("NINIMMA_TEST_PICTURES").expect("set NINIMMA_TEST_PICTURES"),
        );
        let photos = PathBuf::from(
            std::env::var_os("NINIMMA_TEST_PHOTOS").expect("set NINIMMA_TEST_PHOTOS"),
        );
        let truth: &[(&str, &[&str])] = &[
            (
                "mountains",
                &[
                    "mountain-landscape",
                    "snow-mountain-peak",
                    "desert-canyon-arizona",
                ],
            ),
            ("a cat", &["cat-sleeping", "tabby-cat-face"]),
            ("a dog", &["golden-retriever-dog-portrait", "dog-beach"]),
            ("the beach", &["sandy-beach-ocean-waves", "dog-beach"]),
            ("a sunset", &["sailboat", "sunset-over-sea"]),
            (
                "snow",
                &[
                    "forest-trail-trees",
                    "snow-covered-house-winter",
                    "snow-mountain-peak",
                ],
            ),
            ("food", &["hamburger-meal", "pizza", "birthday-cake"]),
            ("a christmas tree", &["christmas-tree-decorated"]),
            ("fireworks", &["fireworks-night"]),
            ("a receipt", &["restaurant-receipt"]),
            ("a waterfall", &["waterfall"]),
            ("a wedding", &["wedding-couple"]),
            ("a bicycle", &["bicycle"]),
            ("a laptop", &["laptop-on-desk"]),
            ("a bridge", &["bridge-river"]),
            ("a horse", &[]),
            ("a car", &[]),
        ];
        let pictures = Pictures::at(
            models.join(crate::models::PICTURE_MODEL.file),
            models.join(crate::models::PICTURE_TOKENIZER.file),
        );
        pictures.set_threads(4);
        let mut names = Vec::new();
        let mut images = Vec::new();
        for entry in std::fs::read_dir(&photos).unwrap().flatten() {
            if let Some(image) = crate::thumbnail::decode_oriented(&entry.path()) {
                names.push(
                    entry
                        .path()
                        .file_stem()
                        .unwrap()
                        .to_string_lossy()
                        .to_string(),
                );
                images.push(image);
            }
        }
        let shots = pictures.pictures(&images).unwrap();
        let (mut tp, mut fp, mut missed) = (0, 0, 0);
        for (query, want) in truth {
            let q = pictures.query(query).unwrap();
            let got: HashSet<&str> = shots
                .iter()
                .zip(&names)
                .filter(|(s, _)| score(&q, &s.vector, s.baseline) >= MATCH_FLOOR)
                .map(|(_, n)| n.as_str())
                .collect();
            let want: HashSet<&str> = want.iter().copied().collect();
            tp += got.intersection(&want).count();
            fp += got.difference(&want).count();
            missed += want.difference(&got).count();
            eprintln!("{query:>18}: {got:?}");
        }
        let precision = tp as f32 / (tp + fp).max(1) as f32;
        let recall = tp as f32 / (tp + missed).max(1) as f32;
        eprintln!("precision {precision:.2}, recall {recall:.2}");
        assert!(
            precision >= 0.85 && recall >= 0.85,
            "precision {precision:.2}, recall {recall:.2}"
        );
    }
}
