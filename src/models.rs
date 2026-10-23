// Ninimma modification: independent project branding and integration.
//! The optional GGUF models for smart search: which ones, where they live,
//! and getting them there safely.
//!
//! Models are downloaded only when the user asks, or copied in by hand on
//! computers without internet access. Every model is checked against its
//! known SHA-256 before llama.cpp ever opens it, because a malformed model
//! file is untrusted input to a C++ parser.

// Builds without smart search keep this code (and its tests) but don't
// call all of it.
#![cfg_attr(not(feature = "smart-search"), allow(dead_code))]

use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

/// A model Ninimma knows how to use, pinned to one exact file.
#[derive(Debug)]
pub struct ModelSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub file: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
    pub license: &'static str,
    pub source: &'static str,
    /// Output size (embedding models).
    pub dimensions: usize,
    /// Added before a search query (some embedding models expect it).
    pub query_prefix: &'static str,
}

/// BGE small (English) v1.5, 8-bit: 384 dimensions, 37 MB, MIT.
pub const EMBEDDING_MODEL: ModelSpec = ModelSpec {
    id: "bge-small-en-v1.5-q8_0",
    name: "BGE small English v1.5 (8-bit)",
    file: "bge-small-en-v1.5-q8_0.gguf",
    url: "https://huggingface.co/ggml-org/bge-small-en-v1.5-Q8_0-GGUF/resolve/main/bge-small-en-v1.5-q8_0.gguf",
    sha256: "f046db1dc724cf4f6f0a0c5917e922823b73eb1d27b8f9a9c2797f7866974804",
    size: 36_685_152,
    license: "MIT",
    source: "https://huggingface.co/BAAI/bge-small-en-v1.5",
    dimensions: 384,
    query_prefix: "Represent this sentence for searching relevant passages: ",
};

/// IBM granite-docling 258M, 8-bit: reads text from images of documents
/// (OCR). Apache-2.0. Two files: the language model and its vision part.
pub const OCR_MODEL: ModelSpec = ModelSpec {
    id: "granite-docling-258M-q8_0",
    name: "IBM granite-docling 258M (8-bit)",
    file: "granite-docling-258M-Q8_0.gguf",
    url: "https://huggingface.co/ggml-org/granite-docling-258M-GGUF/resolve/main/granite-docling-258M-Q8_0.gguf",
    sha256: "b687245c212b59da938e411e2d4485596cbaeed1ae52706582ac38ed6a970f76",
    size: 177_918_144,
    license: "Apache-2.0",
    source: "https://huggingface.co/ibm-granite/granite-docling-258M",
    dimensions: 0,
    query_prefix: "",
};

pub const OCR_PROJECTOR: ModelSpec = ModelSpec {
    id: "mmproj-granite-docling-258M-q8_0",
    name: "IBM granite-docling 258M vision projector (8-bit)",
    file: "mmproj-granite-docling-258M-Q8_0.gguf",
    url: "https://huggingface.co/ggml-org/granite-docling-258M-GGUF/resolve/main/mmproj-granite-docling-258M-Q8_0.gguf",
    sha256: "3d440e65a91e640dc0a53897ad7897700c39324802aaff2072232b6f21d0cadb",
    size: 103_769_792,
    license: "Apache-2.0",
    source: "https://huggingface.co/ibm-granite/granite-docling-258M",
    dimensions: 0,
    query_prefix: "",
};

/// Qwen2.5 0.5B Instruct, 4-bit: picks the important words of a search
/// sentence (experimental). Apache-2.0, 491 MB.
pub const QUERY_MODEL: ModelSpec = ModelSpec {
    id: "qwen2.5-0.5b-instruct-q4_k_m",
    name: "Qwen2.5 0.5B Instruct (4-bit)",
    file: "qwen2.5-0.5b-instruct-q4_k_m.gguf",
    url: "https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct-GGUF/resolve/main/qwen2.5-0.5b-instruct-q4_k_m.gguf",
    sha256: "74a4da8c9fdbcd15bd1f6d01d621410d31c6fc00986f5eb687824e7b93d7a9db",
    size: 491_400_032,
    license: "Apache-2.0",
    source: "https://huggingface.co/Qwen/Qwen2.5-0.5B-Instruct",
    dimensions: 0,
    query_prefix: "",
};

/// OpenAI CLIP ViT-B/32: puts pictures and search words in the same space,
/// so "mountains" finds photos of mountains. MIT, 605 MB (safetensors,
/// read by candle in pure Rust, never by a C++ parser).
pub const PICTURE_MODEL: ModelSpec = ModelSpec {
    id: "clip-vit-base-patch32",
    name: "OpenAI CLIP ViT-B/32",
    file: "clip-vit-base-patch32.safetensors",
    url: "https://huggingface.co/openai/clip-vit-base-patch32/resolve/b33cedfd0df4e43b8238760678fcc89e1a0d38b3/model.safetensors",
    sha256: "99d28a652e6ec46629ab7047a0ac82c69b1fe11e0ce672c43af65d3a9a3fc05d",
    size: 605_157_884,
    license: "MIT",
    source: "https://github.com/openai/CLIP",
    dimensions: 512,
    query_prefix: "a photo of ",
};

pub const PICTURE_TOKENIZER: ModelSpec = ModelSpec {
    id: "clip-vit-base-patch32-tokenizer",
    name: "OpenAI CLIP ViT-B/32 tokenizer",
    file: "clip-vit-base-patch32-tokenizer.json",
    url: "https://huggingface.co/openai/clip-vit-base-patch32/resolve/b33cedfd0df4e43b8238760678fcc89e1a0d38b3/tokenizer.json",
    sha256: "b556ac8c99757ffb677208af34bc8c6721572114111a6e0aaf5fa69ff0b8d842",
    size: 2_224_041,
    license: "MIT",
    source: "https://github.com/openai/CLIP",
    dimensions: 0,
    query_prefix: "",
};

/// Every file the picture model needs.
pub const PICTURE_FILES: [&ModelSpec; 2] = [&PICTURE_MODEL, &PICTURE_TOKENIZER];

/// Every file the OCR model needs.
pub const OCR_FILES: [&ModelSpec; 2] = [&OCR_MODEL, &OCR_PROJECTOR];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelError {
    NoFolder,
    /// The file isn't the expected model (wrong or damaged download).
    WrongChecksum,
    TooLarge,
    Network(String),
    Cancelled,
    Io(io::ErrorKind),
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModelError::NoFolder => f.write_str("there's no per-user folder for models"),
            ModelError::WrongChecksum => {
                f.write_str("the file isn't the expected model (its checksum doesn't match)")
            }
            ModelError::TooLarge => f.write_str("the download is larger than the model should be"),
            ModelError::Network(e) => write!(f, "the download failed: {e}"),
            ModelError::Cancelled => f.write_str("the download was cancelled"),
            ModelError::Io(kind) => {
                write!(f, "the model file couldn't be written or read ({kind})")
            }
        }
    }
}

impl From<io::Error> for ModelError {
    fn from(e: io::Error) -> Self {
        ModelError::Io(e.kind())
    }
}

/// Where models live: per user, local, not synced.
pub fn models_dir() -> Option<PathBuf> {
    // Tests never see or change the user's installed models: with a real
    // OCR model installed, a test expecting none saw OCR start (and could
    // have loaded it). The opt-in model tests name their files with
    // NINIMMA_TEST_*; everything else gets an empty folder per run.
    if cfg!(test) {
        return Some(
            std::env::temp_dir().join(format!("ninimma-test-models-{}", std::process::id())),
        );
    }
    let env = |n: &str| {
        std::env::var_os(n)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    if cfg!(windows) {
        Some(env("LOCALAPPDATA")?.join("Ninimma").join("Models"))
    } else if cfg!(target_os = "macos") {
        Some(env("HOME")?.join("Library/Application Support/Ninimma/Models"))
    } else {
        let data = env("XDG_DATA_HOME").or_else(|| env("HOME").map(|h| h.join(".local/share")))?;
        Some(data.join("ninimma").join("models"))
    }
}

pub fn path_of(spec: &ModelSpec) -> Option<PathBuf> {
    Some(models_dir()?.join(spec.file))
}

/// Whether the model file is there (right size; the checksum is checked
/// when it's first loaded).
pub fn is_installed(spec: &ModelSpec) -> bool {
    path_of(spec)
        .and_then(|p| fs::metadata(p).ok())
        .is_some_and(|m| m.is_file() && m.len() == spec.size)
}

/// Files already checked in this run, by path, size and modified time.
static VERIFIED: Mutex<Vec<(PathBuf, u64, Option<std::time::SystemTime>)>> = Mutex::new(Vec::new());

/// Checks a model file against its pinned SHA-256 (once per run per file
/// version), before anything parses it.
pub fn verify(spec: &ModelSpec, path: &Path) -> Result<(), ModelError> {
    let meta = fs::metadata(path)?;
    let key = (path.to_path_buf(), meta.len(), meta.modified().ok());
    if VERIFIED.lock().unwrap().contains(&key) {
        return Ok(());
    }
    if meta.len() != spec.size {
        return Err(ModelError::WrongChecksum);
    }
    let mut hasher = Sha256::new();
    let mut file = File::open(path)?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    if hex(&hasher.finalize()) != spec.sha256 {
        return Err(ModelError::WrongChecksum);
    }
    VERIFIED.lock().unwrap().push(key);
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Progress of a download, shared with the UI.
#[derive(Default)]
pub struct Progress {
    pub done: AtomicU64,
    pub cancel: AtomicBool,
}

/// Downloads a model over HTTPS, checking its size and SHA-256 as it
/// arrives. The file only appears under its real name once it's verified.
pub fn download(spec: &ModelSpec, progress: &Progress) -> Result<PathBuf, ModelError> {
    let response = ureq::get(spec.url)
        .call()
        .map_err(|e| ModelError::Network(short_error(&e.to_string())))?;
    let reader = response.into_body().into_reader();
    save_verified(
        &models_dir().ok_or(ModelError::NoFolder)?,
        spec,
        reader,
        progress,
    )
}

/// Installs a model the user copied over themselves (e.g. on a computer
/// without internet), after checking it.
pub fn install_from(spec: &ModelSpec, source: &Path) -> Result<PathBuf, ModelError> {
    let dir = models_dir().ok_or(ModelError::NoFolder)?;
    save_verified(&dir, spec, File::open(source)?, &Progress::default())
}

fn save_verified(
    dir: &Path,
    spec: &ModelSpec,
    mut reader: impl Read,
    progress: &Progress,
) -> Result<PathBuf, ModelError> {
    fs::create_dir_all(dir)?;
    let target = dir.join(spec.file);
    let partial = dir.join(format!("{}.part", spec.file));
    let _ = fs::remove_file(&partial);
    let mut out = File::options()
        .write(true)
        .create_new(true)
        .open(&partial)?;
    let result = (|| {
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 16];
        let mut total = 0u64;
        loop {
            if progress.cancel.load(Ordering::Relaxed) {
                return Err(ModelError::Cancelled);
            }
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > spec.size {
                return Err(ModelError::TooLarge);
            }
            hasher.update(&buf[..n]);
            out.write_all(&buf[..n])?;
            progress.done.store(total, Ordering::Relaxed);
        }
        out.sync_all()?;
        if total != spec.size || hex(&hasher.finalize()) != spec.sha256 {
            return Err(ModelError::WrongChecksum);
        }
        Ok(())
    })();
    drop(out);
    match result {
        Ok(()) => {
            fs::rename(&partial, &target)?;
            Ok(target)
        }
        Err(e) => {
            let _ = fs::remove_file(&partial);
            Err(e)
        }
    }
}

pub fn remove(spec: &ModelSpec) -> Result<(), ModelError> {
    let path = path_of(spec).ok_or(ModelError::NoFolder)?;
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Network errors can be long; keep the first line, briefly.
fn short_error(e: &str) -> String {
    let line = e.lines().next().unwrap_or(e);
    line.chars().take(120).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tests_never_use_the_users_installed_models() {
        let dir = models_dir().unwrap();
        assert!(dir.starts_with(std::env::temp_dir()), "{dir:?}");
        assert!(!is_installed(&EMBEDDING_MODEL) && !is_installed(&OCR_MODEL));
    }

    const TINY: ModelSpec = ModelSpec {
        id: "tiny",
        name: "tiny",
        file: "tiny.gguf",
        url: "",
        // sha256("hello")
        sha256: "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
        size: 5,
        license: "",
        source: "",
        dimensions: 1,
        query_prefix: "",
    };

    #[test]
    fn checksums_are_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good");
        fs::write(&good, b"hello").unwrap();
        verify(&TINY, &good).unwrap();
        let bad = dir.path().join("bad");
        fs::write(&bad, b"hellO").unwrap();
        assert_eq!(verify(&TINY, &bad), Err(ModelError::WrongChecksum));
        let long = dir.path().join("long");
        fs::write(&long, b"hello!").unwrap();
        assert_eq!(verify(&TINY, &long), Err(ModelError::WrongChecksum));
    }

    #[test]
    fn saving_keeps_only_verified_files() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let target = save_verified(d, &TINY, &b"hello"[..], &Progress::default()).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"hello");
        fs::remove_file(&target).unwrap();
        assert_eq!(
            save_verified(d, &TINY, &b"hellO"[..], &Progress::default()),
            Err(ModelError::WrongChecksum)
        );
        assert_eq!(
            save_verified(d, &TINY, &b"hello world"[..], &Progress::default()),
            Err(ModelError::TooLarge)
        );
        let leftovers: Vec<_> = fs::read_dir(d).unwrap().collect();
        assert!(
            leftovers.is_empty(),
            "nothing unverified is left behind: {leftovers:?}"
        );
        let cancelled = Progress::default();
        cancelled.cancel.store(true, Ordering::Relaxed);
        assert_eq!(
            save_verified(d, &TINY, &b"hello"[..], &cancelled),
            Err(ModelError::Cancelled)
        );
    }

    #[test]
    fn every_model_file_is_pinned() {
        for spec in [&EMBEDDING_MODEL, &OCR_MODEL, &OCR_PROJECTOR, &QUERY_MODEL] {
            assert_eq!(spec.sha256.len(), 64, "{}", spec.id);
            assert!(spec
                .sha256
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
            assert!(
                spec.url.starts_with("https://huggingface.co/ggml-org/")
                    || spec.url.starts_with("https://huggingface.co/Qwen/"),
                "{}",
                spec.id
            );
            assert!(spec.url.ends_with(spec.file));
        }
    }

    #[test]
    fn the_embedding_model_is_pinned() {
        assert_eq!(EMBEDDING_MODEL.sha256.len(), 64);
        assert!(EMBEDDING_MODEL.url.starts_with("https://"));
        assert_eq!(EMBEDDING_MODEL.dimensions, 384);
    }
}
