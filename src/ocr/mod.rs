// Ninimma modification: independent project branding and integration.
//! Reading the text in photos of documents and in scanned PDFs (OCR), so
//! a photographed receipt can be found by what's printed on it.
//!
//! [`local`] runs IBM's granite-docling (a small document-reading vision
//! model, Apache-2.0) through llama.cpp in this process. Only images that
//! look like paper are read (see [`looks_like_a_document`]): reading one
//! takes ~20 s on two CPU threads, so a photo library isn't fed through it.
//! Images are decoded by the app's own Rust decoders (with size limits),
//! never by the C image loader bundled with llama.cpp.

// Builds without smart search keep this code (and its tests) but don't
// call all of it.
#![cfg_attr(not(feature = "smart-search"), allow(dead_code))]

#[cfg(feature = "smart-search")]
pub mod local;

use crate::models::{self, ModelSpec};
use image::{DynamicImage, RgbImage};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Images are scaled so their longer side is this many pixels. The model
/// tiles every image the same way, so this costs no time; at the original
/// size of one test receipt it stopped after five lines, at 512 and 1024
/// it read everything, every time.
pub const IMAGE_SIDE: u32 = 1024;
/// Scanned PDFs: at most this many pages are read.
pub const PDF_PAGES: u32 = 2;
/// At most this much text is kept per image.
pub const MAX_CHARS: usize = 20_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcrError {
    #[cfg_attr(feature = "smart-search", allow(dead_code))]
    NotBuiltIn,
    NotInstalled,
    Model(String),
    Failed(&'static str),
}

impl fmt::Display for OcrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OcrError::NotBuiltIn => {
                f.write_str("reading text in images isn't included in this build")
            }
            OcrError::NotInstalled => f.write_str("the text-reading model isn't installed"),
            OcrError::Model(e) => write!(f, "the text-reading model can't be used: {e}"),
            OcrError::Failed(e) => write!(f, "reading the text failed: {e}"),
        }
    }
}

/// Reads the text in one prepared image.
pub trait TextReader: Send {
    fn read(&mut self, image: &RgbImage) -> Result<String, OcrError>;
}

/// The shared, lazily loaded OCR model.
pub struct Ocr {
    /// Model files elsewhere than the models folder (tests).
    paths: Option<(PathBuf, PathBuf)>,
    threads: Mutex<i32>,
    loaded: Mutex<Option<(Box<dyn TextReader>, Instant)>>,
}

impl Ocr {
    pub fn new() -> Self {
        Self {
            paths: None,
            threads: Mutex::new(1),
            loaded: Mutex::new(None),
        }
    }

    pub fn at(model: PathBuf, projector: PathBuf) -> Self {
        Self {
            paths: Some((model, projector)),
            ..Self::new()
        }
    }

    fn model_paths(&self) -> Option<(PathBuf, PathBuf)> {
        let (model, projector) = match &self.paths {
            Some(p) => p.clone(),
            None => (
                models::path_of(&models::OCR_MODEL)?,
                models::path_of(&models::OCR_PROJECTOR)?,
            ),
        };
        let ok = |p: &Path, spec: &ModelSpec| {
            std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.len() == spec.size)
        };
        (ok(&model, &models::OCR_MODEL) && ok(&projector, &models::OCR_PROJECTOR))
            .then_some((model, projector))
    }

    pub fn is_available(&self) -> bool {
        crate::embedding::is_built_in() && self.model_paths().is_some()
    }

    pub fn set_threads(&self, threads: usize) {
        *self.threads.lock().unwrap() = threads.clamp(1, 8) as i32;
    }

    /// Reads the text in `image` (any size; it's scaled first).
    pub fn read(&self, image: &DynamicImage) -> Result<String, OcrError> {
        let prepared = prepare(image);
        let mut loaded = self.loaded.lock().unwrap();
        if loaded.is_none() {
            *loaded = Some((self.load()?, Instant::now()));
        }
        let (reader, used) = loaded.as_mut().expect("loaded");
        *used = Instant::now();
        let raw = reader.read(&prepared)?;
        Ok(clean(&raw))
    }

    #[cfg(feature = "smart-search")]
    fn load(&self) -> Result<Box<dyn TextReader>, OcrError> {
        let (model, projector) = self.model_paths().ok_or(OcrError::NotInstalled)?;
        models::verify(&models::OCR_MODEL, &model).map_err(|e| OcrError::Model(e.to_string()))?;
        models::verify(&models::OCR_PROJECTOR, &projector)
            .map_err(|e| OcrError::Model(e.to_string()))?;
        let threads = *self.threads.lock().unwrap();
        Ok(Box::new(local::GraniteOcr::load(
            &model, &projector, threads,
        )?))
    }

    #[cfg(not(feature = "smart-search"))]
    fn load(&self) -> Result<Box<dyn TextReader>, OcrError> {
        Err(OcrError::NotBuiltIn)
    }

    pub fn unload(&self) {
        *self.loaded.lock().unwrap() = None;
    }

    #[allow(dead_code)]
    pub fn idle_for(&self) -> Option<Duration> {
        self.loaded
            .lock()
            .unwrap()
            .as_ref()
            .map(|(_, used)| used.elapsed())
    }
}

/// Scales an image so its longer side is `IMAGE_SIDE`, in RGB.
pub fn prepare(image: &DynamicImage) -> RgbImage {
    let (w, h) = (image.width().max(1), image.height().max(1));
    let scale = IMAGE_SIDE as f32 / w.max(h) as f32;
    let (nw, nh) = (
        ((w as f32 * scale).round() as u32).max(1),
        ((h as f32 * scale).round() as u32).max(1),
    );
    image
        .resize_exact(nw, nh, image::imageops::FilterType::Triangle)
        .to_rgb8()
}

/// Whether an image looks like paper with writing on it (a receipt, a
/// letter, a scan) rather than a scene. Cheap: it looks at a 256-pixel
/// copy. Paper is light and colourless; writing adds marks clearly darker
/// than the paper (at this size thin text is grey rather than black).
pub fn looks_like_a_document(image: &DynamicImage) -> bool {
    let small = image.thumbnail(256, 256).to_rgb8();
    let total = (small.width() * small.height()).max(1) as f32;
    let (mut paper, mut marks) = (0u32, 0u32);
    for p in small.pixels() {
        let [r, g, b] = p.0.map(|c| c as f32 / 255.0);
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let saturation = if max > 0.0 { (max - min) / max } else { 0.0 };
        if max > 0.6 && saturation < 0.2 {
            if max < 0.8 {
                marks += 1; // light grey: text strokes blurred by scaling
            } else {
                paper += 1;
            }
        } else if max <= 0.6 && saturation < 0.35 {
            marks += 1; // dark, colourless: ink
        }
    }
    let (paper, marks) = (paper as f32 / total, marks as f32 / total);
    paper > 0.35 && (0.01..0.5).contains(&marks)
}

/// The model writes DocTags (layout markup such as `<loc_60>` and
/// `<_text_>`); only the words matter here. Also drops lines repeated over
/// and over (small models can get stuck), and bounds the length.
pub fn clean(raw: &str) -> String {
    let mut text = String::with_capacity(raw.len());
    let mut in_tag = false;
    for c in raw.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                text.push(' ');
            }
            _ if in_tag => {}
            c => text.push(c),
        }
    }
    let mut out = String::new();
    let mut seen: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    for line in text.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() {
            continue;
        }
        let count = seen.entry(line.clone()).or_default();
        *count += 1;
        if *count > 2 {
            continue;
        }
        out.push_str(&line);
        out.push('\n');
        if out.len() > MAX_CHARS {
            break;
        }
    }
    if let Some((cut, _)) = out.char_indices().nth(MAX_CHARS) {
        out.truncate(cut);
    }
    out.trim_end().to_string()
}

/// Renders the first pages of a PDF to images with Poppler's `pdftoppm`
/// (fixed arguments, no shell, killed after `limit`), into `work`, a
/// private folder. Returns the images in page order.
pub fn render_pdf(pdf: &Path, work: &Path, limit: Duration) -> Option<Vec<DynamicImage>> {
    let tool = find_tool(if cfg!(windows) {
        "pdftoppm.exe"
    } else {
        "pdftoppm"
    })?;
    std::fs::create_dir_all(work).ok()?;
    let prefix = work.join("page");
    let mut child = Command::new(tool)
        .args([
            "-f",
            "1",
            "-l",
            &PDF_PAGES.to_string(),
            "-scale-to",
            &IMAGE_SIDE.to_string(),
            "-png",
        ])
        .arg(pdf)
        .arg(&prefix)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + limit;
    let finished = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => break false,
        }
    };
    let mut pages: Vec<PathBuf> = std::fs::read_dir(work)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "png"))
        .collect();
    pages.sort();
    let images = finished.then(|| pages.iter().filter_map(|p| image::open(p).ok()).collect());
    for p in pages {
        let _ = std::fs::remove_file(p);
    }
    images
}

pub fn find_tool(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    #[test]
    fn doctags_are_reduced_to_words() {
        let raw = "<loc_60><loc_26><loc_499><loc_183><_YAML_>RED ROCK LODGE\n  Sedona, AZ 86336\n  TOTAL   $412.50\n<end_of_utterance>";
        assert_eq!(
            clean(raw),
            "RED ROCK LODGE\nSedona, AZ 86336\nTOTAL $412.50"
        );
    }

    #[test]
    fn stuck_output_is_cut_short() {
        let raw = "Total 12\n".repeat(50) + &"x".repeat(MAX_CHARS * 2);
        let text = clean(&raw);
        assert_eq!(text.matches("Total 12").count(), 2);
        assert!(text.chars().count() <= MAX_CHARS);
    }

    #[test]
    fn paper_with_ink_looks_like_a_document_and_a_scene_does_not() {
        // White paper with lines of dark text.
        let mut page = RgbImage::from_pixel(300, 500, Rgb([245, 245, 240]));
        for row in (40..400).step_by(30) {
            for y in row..row + 8 {
                for x in 30..260 {
                    if (x / 6) % 3 != 0 {
                        page.put_pixel(x, y, Rgb([20, 20, 20]));
                    }
                }
            }
        }
        assert!(looks_like_a_document(&DynamicImage::ImageRgb8(page)));
        // Red rocks under a blue sky.
        let mut scene = RgbImage::from_pixel(300, 200, Rgb([190, 80, 40]));
        for y in 0..80 {
            for x in 0..300 {
                scene.put_pixel(x, y, Rgb([90, 150, 230]));
            }
        }
        assert!(!looks_like_a_document(&DynamicImage::ImageRgb8(scene)));
        // The rendered receipt, at its real size.
        if find_tool("pdftoppm").is_some() {
            assert!(looks_like_a_document(&super::tests_support::receipt_image()));
        }
        // A blank white image has no text.
        assert!(!looks_like_a_document(&DynamicImage::ImageRgb8(
            RgbImage::from_pixel(100, 100, Rgb([255, 255, 255]))
        )));
    }

    #[test]
    fn images_are_scaled_to_the_model_size() {
        let tall = DynamicImage::new_rgb8(534, 934);
        let p = prepare(&tall);
        assert_eq!(p.height(), IMAGE_SIDE);
        assert_eq!(p.width(), 585);
        let small = DynamicImage::new_rgb8(100, 50);
        assert_eq!(prepare(&small).width(), IMAGE_SIDE);
    }

    #[test]
    fn scanned_pdfs_are_rendered_when_poppler_is_installed() {
        if find_tool("pdftoppm").is_none() {
            return;
        }
        let pdf = b"%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 300 100]/Contents 4 0 R/Resources<</Font<</F1 5 0 R>>>>>>endobj\n4 0 obj<</Length 44>>stream\nBT /F1 12 Tf 20 50 Td (Receipt Phoenix) Tj ET\nendstream endobj\n5 0 obj<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>endobj\ntrailer<</Root 1 0 R>>\n%%EOF\n";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scan.pdf");
        std::fs::write(&path, pdf).unwrap();
        let work = dir.path().join("work");
        let pages = render_pdf(&path, &work, Duration::from_secs(20)).unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].width().max(pages[0].height()), IMAGE_SIDE);
        assert_eq!(
            std::fs::read_dir(&work).unwrap().count(),
            0,
            "rendered pages are removed"
        );
    }
}

/// A test receipt, as a real PDF and as a rendered image.
#[cfg(test)]
pub(crate) mod tests_support {
    use image::DynamicImage;

    pub const RECEIPT_LINES: &[&str] = &[
        "RED ROCK LODGE",
        "1200 W State Route 89A",
        "Sedona, AZ 86336",
        "Date: 12/20/2025  14:31",
        "Room 214 - 2 nights   $380.00",
        "Breakfast            $32.50",
        "TOTAL                $412.50",
        "VISA ****4417   APPROVED",
        "Thank you for staying!",
    ];

    /// A one-page PDF printing the receipt in a typewriter font.
    pub fn receipt_pdf() -> Vec<u8> {
        let mut stream = String::from("BT /F1 16 Tf 40 520 Td 20 TL\n");
        for line in RECEIPT_LINES {
            stream.push_str(&format!("({line}) Tj T*\n"));
        }
        stream.push_str("ET");
        let objects = [
            "<</Type/Catalog/Pages 2 0 R>>".to_string(),
            "<</Type/Pages/Kids[3 0 R]/Count 1>>".to_string(),
            "<</Type/Page/Parent 2 0 R/MediaBox[0 0 320 560]/Contents 4 0 R/Resources<</Font<</F1 5 0 R>>>>>>".to_string(),
            format!("<</Length {}>>stream\n{stream}\nendstream", stream.len()),
            "<</Type/Font/Subtype/Type1/BaseFont/Courier-Bold>>".to_string(),
        ];
        let mut out = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (i, o) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.push_str(&format!("{} 0 obj\n{o}\nendobj\n", i + 1));
        }
        let xref = out.len();
        out.push_str(&format!(
            "xref\n0 {}\n0000000000 65535 f \n",
            objects.len() + 1
        ));
        for o in offsets {
            out.push_str(&format!("{o:010} 00000 n \n"));
        }
        out.push_str(&format!(
            "trailer<</Size {}/Root 1 0 R>>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        ));
        out.into_bytes()
    }

    /// The receipt rendered like a scan (needs Poppler's pdftoppm).
    pub fn receipt_image() -> DynamicImage {
        let dir = tempfile::tempdir().unwrap();
        let pdf = dir.path().join("receipt.pdf");
        std::fs::write(&pdf, receipt_pdf()).unwrap();
        super::render_pdf(
            &pdf,
            &dir.path().join("work"),
            std::time::Duration::from_secs(30),
        )
        .expect("pdftoppm renders the receipt")
        .remove(0)
    }
}
