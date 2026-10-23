// Ninimma modification: independent project branding and integration.
//! Searching what's in pictures: "mountains" finds photos of mountains,
//! "a cat" videos of a cat, without either word in a name.
//!
//! [`local`] runs OpenAI's CLIP ViT-B/32 (MIT) with candle, in pure Rust,
//! in this process. CLIP turns a picture and a piece of text into points in
//! the same space; a picture and a description of it land close together.
//! Each photo (and a few frames of each video) gets its point once, in the
//! background (~40 ms a photo); a search compares the words' point with
//! all of them.
//!
//! Some pictures land close to *every* description (CLIP's "hubs": a starry
//! sky scored high for "a cat"). So each picture also keeps how close it is
//! to a description of nothing in particular ("a photo"), and a match is
//! how much closer the search is than that. Measured on 36 labelled photos
//! (`local::tests`), a match floor of 0.02 kept 93% precision at 98% recall.

// Builds without smart search keep this code (and its tests) but don't
// call all of it.
#![cfg_attr(not(feature = "smart-search"), allow(dead_code))]

#[cfg(feature = "smart-search")]
pub mod local;

use crate::models::{self, ModelSpec};
use image::{DynamicImage, RgbImage};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Pictures are fed to the model at this size (CLIP's own).
pub const IMAGE_SIDE: u32 = 224;
/// A picture matches a search this much closer than "a photo".
pub const MATCH_FLOOR: f32 = 0.02;
/// Pictures embedded per model call.
pub const BATCH: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PictureError {
    #[cfg_attr(feature = "smart-search", allow(dead_code))]
    NotBuiltIn,
    NotInstalled,
    Model(String),
}

impl fmt::Display for PictureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PictureError::NotBuiltIn => {
                f.write_str("searching pictures isn't included in this build")
            }
            PictureError::NotInstalled => f.write_str("the picture model isn't installed"),
            PictureError::Model(e) => write!(f, "the picture model can't be used: {e}"),
        }
    }
}

/// One picture's point, and how close it is to "a photo" of anything.
#[derive(Debug, Clone, PartialEq)]
pub struct Picture {
    pub vector: Vec<f32>,
    pub baseline: f32,
}

/// How well a picture matches a search's point, as stored (8-bit, see
/// `embedding::quantize`): above [`MATCH_FLOOR`] is a match.
#[cfg(test)]
pub fn score(query: &[f32], picture: &[f32], baseline: f32) -> f32 {
    let stored = crate::embedding::quantize(picture);
    crate::embedding::similarity(query, &stored).unwrap_or(f32::MIN) - baseline
}

/// The model itself: points for prepared pictures, and for texts.
pub trait PictureModel: Send {
    fn pictures(&mut self, images: &[RgbImage]) -> Result<Vec<Vec<f32>>, PictureError>;
    fn texts(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, PictureError>;
}

struct Loaded {
    model: Box<dyn PictureModel>,
    /// Where "a photo" of nothing in particular lands (unit length).
    neutral: Vec<f32>,
    used: Instant,
}

/// Descriptions of nothing in particular, averaged for the baseline.
const NEUTRAL: [&str; 4] = ["a photo", "a picture", "an image", "a photo of something"];

/// The shared, lazily loaded picture model.
pub struct Pictures {
    /// Model files elsewhere than the models folder (tests).
    paths: Option<(PathBuf, PathBuf)>,
    threads: Mutex<usize>,
    loaded: Mutex<Option<Loaded>>,
    /// The files' checksums were checked (once a run: the model is 605 MB
    /// and is loaded again after sitting unused).
    verified: Mutex<Option<(PathBuf, std::time::SystemTime)>>,
}

impl Pictures {
    pub fn new() -> Self {
        Self {
            paths: None,
            threads: Mutex::new(1),
            loaded: Mutex::new(None),
            verified: Mutex::new(None),
        }
    }

    #[allow(dead_code)] // opt-in model tests
    pub fn at(model: PathBuf, tokenizer: PathBuf) -> Self {
        Self {
            paths: Some((model, tokenizer)),
            ..Self::new()
        }
    }

    fn model_paths(&self) -> Option<(PathBuf, PathBuf)> {
        let (model, tokenizer) = match &self.paths {
            Some(p) => p.clone(),
            None => (
                models::path_of(&models::PICTURE_MODEL)?,
                models::path_of(&models::PICTURE_TOKENIZER)?,
            ),
        };
        let ok = |p: &Path, spec: &ModelSpec| {
            std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.len() == spec.size)
        };
        (ok(&model, &models::PICTURE_MODEL) && ok(&tokenizer, &models::PICTURE_TOKENIZER))
            .then_some((model, tokenizer))
    }

    pub fn is_available(&self) -> bool {
        crate::embedding::is_built_in() && self.model_paths().is_some()
    }

    pub fn set_threads(&self, threads: usize) {
        *self.threads.lock().unwrap() = threads.clamp(1, 8);
    }

    fn with_model<T>(
        &self,
        f: impl FnOnce(&mut Loaded) -> Result<T, PictureError>,
    ) -> Result<T, PictureError> {
        let mut loaded = self.loaded.lock().unwrap();
        if loaded.is_none() {
            let mut model = self.load()?;
            let neutral = model.texts(&NEUTRAL.map(String::from))?;
            *loaded = Some(Loaded {
                neutral: unit(mean(&neutral)),
                model,
                used: Instant::now(),
            });
        }
        let l = loaded.as_mut().expect("loaded");
        l.used = Instant::now();
        f(l)
    }

    /// Points for pictures (any size; they're scaled first).
    pub fn pictures(&self, images: &[DynamicImage]) -> Result<Vec<Picture>, PictureError> {
        let prepared: Vec<RgbImage> = images.iter().map(prepare).collect();
        self.with_model(|l| {
            let mut out = Vec::with_capacity(prepared.len());
            for chunk in prepared.chunks(BATCH) {
                for vector in l.model.pictures(chunk)? {
                    let vector = unit(vector);
                    let baseline = dot(&vector, &l.neutral);
                    out.push(Picture { vector, baseline });
                }
            }
            Ok(out)
        })
    }

    /// One point for several frames of a video: their average.
    pub fn video(&self, frames: &[DynamicImage]) -> Result<Option<Picture>, PictureError> {
        let points = self.pictures(frames)?;
        if points.is_empty() {
            return Ok(None);
        }
        let vector = unit(mean(
            &points.iter().map(|p| p.vector.clone()).collect::<Vec<_>>(),
        ));
        let baseline = self.with_model(|l| Ok(dot(&vector, &l.neutral)))?;
        Ok(Some(Picture { vector, baseline }))
    }

    /// The point for a search's words ("mountains" → "a photo of mountains").
    pub fn query(&self, words: &str) -> Result<Vec<f32>, PictureError> {
        let text = format!("{}{words}", models::PICTURE_MODEL.query_prefix);
        self.with_model(|l| Ok(unit(l.model.texts(&[text])?.remove(0))))
    }

    #[cfg(feature = "smart-search")]
    fn load(&self) -> Result<Box<dyn PictureModel>, PictureError> {
        let (model, tokenizer) = self.model_paths().ok_or(PictureError::NotInstalled)?;
        // Checked again only if the file was replaced since.
        let stamp = std::fs::metadata(&model)
            .and_then(|m| m.modified())
            .map_err(|e| PictureError::Model(e.to_string()))?;
        let mut verified = self.verified.lock().unwrap();
        if verified.as_ref() != Some(&(model.clone(), stamp)) {
            models::verify(&models::PICTURE_MODEL, &model)
                .map_err(|e| PictureError::Model(e.to_string()))?;
            *verified = Some((model.clone(), stamp));
        }
        drop(verified);
        models::verify(&models::PICTURE_TOKENIZER, &tokenizer)
            .map_err(|e| PictureError::Model(e.to_string()))?;
        let threads = *self.threads.lock().unwrap();
        Ok(Box::new(local::Clip::load(&model, &tokenizer, threads)?))
    }

    #[cfg(not(feature = "smart-search"))]
    fn load(&self) -> Result<Box<dyn PictureModel>, PictureError> {
        Err(PictureError::NotBuiltIn)
    }

    pub fn unload(&self) {
        *self.loaded.lock().unwrap() = None;
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded.lock().unwrap().is_some()
    }

    /// Unloads the model once it's sat unused this long (it holds ~600 MB).
    pub fn unload_if_idle(&self, idle: Duration) {
        let mut loaded = self.loaded.lock().unwrap();
        if loaded.as_ref().is_some_and(|l| l.used.elapsed() >= idle) {
            *loaded = None;
        }
    }
}

/// Scaled and centre-cropped to the model's square, as CLIP was trained.
pub fn prepare(image: &DynamicImage) -> RgbImage {
    image
        .resize_to_fill(
            IMAGE_SIDE,
            IMAGE_SIDE,
            image::imageops::FilterType::Triangle,
        )
        .to_rgb8()
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn mean(vectors: &[Vec<f32>]) -> Vec<f32> {
    let n = vectors.len().max(1) as f32;
    let mut out = vec![0.0; vectors.first().map_or(0, Vec::len)];
    for v in vectors {
        for (o, x) in out.iter_mut().zip(v) {
            *o += x / n;
        }
    }
    out
}

fn unit(mut v: Vec<f32>) -> Vec<f32> {
    let len = dot(&v, &v).sqrt();
    if len > 0.0 {
        v.iter_mut().for_each(|x| *x /= len);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in model: a picture's point is its average colour, a text's
    /// the colour it names, so matching can be tested without the model.
    struct Colours;

    impl PictureModel for Colours {
        fn pictures(&mut self, images: &[RgbImage]) -> Result<Vec<Vec<f32>>, PictureError> {
            Ok(images
                .iter()
                .map(|i| {
                    let p = i.get_pixel(0, 0).0;
                    vec![p[0] as f32, p[1] as f32, p[2] as f32, 40.0]
                })
                .collect())
        }
        fn texts(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>, PictureError> {
            Ok(texts
                .iter()
                .map(|t| match t.rsplit(' ').next().unwrap() {
                    "red" => vec![255.0, 0.0, 0.0, 40.0],
                    "blue" => vec![0.0, 0.0, 255.0, 40.0],
                    _ => vec![1.0, 1.0, 1.0, 1.0],
                })
                .collect())
        }
    }

    fn with_stand_in() -> Pictures {
        let p = Pictures::new();
        let mut model: Box<dyn PictureModel> = Box::new(Colours);
        let neutral = model.texts(&NEUTRAL.map(String::from)).unwrap();
        *p.loaded.lock().unwrap() = Some(Loaded {
            neutral: unit(mean(&neutral)),
            model,
            used: Instant::now(),
        });
        p
    }

    fn plain(rgb: [u8; 3]) -> DynamicImage {
        DynamicImage::ImageRgb8(RgbImage::from_pixel(300, 200, image::Rgb(rgb)))
    }

    #[test]
    fn a_search_matches_the_picture_it_describes_more_than_any_picture() {
        let p = with_stand_in();
        let shots = p
            .pictures(&[plain([250, 10, 10]), plain([10, 10, 250])])
            .unwrap();
        let red = p.query("red").unwrap();
        let red_score = score(&red, &shots[0].vector, shots[0].baseline);
        let blue_score = score(&red, &shots[1].vector, shots[1].baseline);
        assert!(red_score > MATCH_FLOOR, "{red_score}");
        assert!(blue_score < MATCH_FLOOR, "{blue_score}");
        // A video is its frames' average.
        let clip = p
            .video(&[plain([250, 10, 10]), plain([240, 20, 0])])
            .unwrap()
            .unwrap();
        assert!(score(&red, &clip.vector, clip.baseline) > MATCH_FLOOR);
        assert_eq!(p.video(&[]).unwrap(), None);
    }

    #[test]
    fn idle_models_are_unloaded() {
        let p = with_stand_in();
        p.unload_if_idle(Duration::from_secs(60));
        assert!(p.loaded.lock().unwrap().is_some());
        p.unload_if_idle(Duration::ZERO);
        assert!(p.loaded.lock().unwrap().is_none());
    }

    #[test]
    fn without_the_model_nothing_is_available() {
        // Tests get an empty models folder.
        assert!(!Pictures::new().is_available());
        assert!(Pictures::new().query("cat").is_err());
    }
}
