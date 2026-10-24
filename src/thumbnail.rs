// Ninimma modification: independent project branding and integration.
//! Asynchronous thumbnail generation with memory and disk caching.
//!
//! A small pool of worker threads decodes images; the UI thread only uploads
//! finished pixels to the GPU. Requests are served newest-first so that
//! whatever is on screen right now wins over items the user scrolled past.

use egui::{ColorImage, Context, TextureFilter, TextureHandle, TextureOptions};
use image::{DynamicImage, ImageDecoder, ImageReader};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

// Cache the ffmpeg path once we find it
static FFMPEG_PATH: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();

/// Pixel size of icon thumbnails. Icon views zoom up to this size; smaller
/// sizes are drawn from the same texture via mipmaps.
pub const THUMBNAIL_SIZE: u32 = 256;
const PREVIEW_SIZE: u32 = 768;

/// Most textures kept in GPU memory per size class before least-recently-used
/// ones are evicted (256px RGBA ≈ 256 KB each).
const MAX_THUMBNAILS: usize = 600;
const MAX_PREVIEWS: usize = 8;
/// Queued jobs beyond this are dropped oldest-first (they will be re-requested
/// if they scroll back into view).
const MAX_QUEUE: usize = 400;
/// Disk cache entries unused for this long are pruned at startup.
const DISK_CACHE_MAX_AGE: Duration = Duration::from_secs(60 * 60 * 24 * 90);
const SOURCE_SIZE_LIMIT: u64 = 200_000_000;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ThumbSize {
    Icon,
    Preview,
}

impl ThumbSize {
    fn pixels(self) -> u32 {
        match self {
            ThumbSize::Icon => THUMBNAIL_SIZE,
            ThumbSize::Preview => PREVIEW_SIZE,
        }
    }
    fn suffix(self) -> &'static str {
        match self {
            ThumbSize::Icon => "t",
            ThumbSize::Preview => "p",
        }
    }
}

type Key = (PathBuf, ThumbSize);

enum Entry {
    /// Queued or being generated; the number identifies this request.
    Loading(u64),
    Ready {
        texture: Option<TextureHandle>,
        last_used: u64,
    },
}

struct Job {
    key: Key,
    generation: u64,
}

struct Finished {
    key: Key,
    generation: u64,
    image: Option<ColorImage>,
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    shutdown: bool,
}

struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
    finished: Mutex<Vec<Finished>>,
    ctx: Mutex<Option<Context>>,
}

pub struct ThumbnailCache {
    entries: HashMap<Key, Entry>,
    shared: Arc<Shared>,
    frame: u64,
    /// Id of the most recent request; a result is only accepted if it
    /// answers the request currently recorded for its key.
    generation: u64,
}

impl ThumbnailCache {
    pub fn new() -> Self {
        Self::with_cache_dir(Self::get_cache_directory())
    }

    pub fn with_cache_dir(cache_dir: PathBuf) -> Self {
        let _ = fs::create_dir_all(&cache_dir);

        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            wake: Condvar::new(),
            finished: Mutex::new(Vec::new()),
            ctx: Mutex::new(None),
        });
        let workers = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(2, 6);
        for _ in 0..workers {
            let shared = shared.clone();
            let cache_dir = cache_dir.clone();
            thread::spawn(move || worker_loop(&shared, &cache_dir));
        }
        thread::spawn(move || prune_disk_cache(&cache_dir));

        Self {
            entries: HashMap::new(),
            shared,
            frame: 0,
            generation: 0,
        }
    }

    fn get_cache_directory() -> PathBuf {
        // Windows: %LOCALAPPDATA%\Ninimma\thumbnails
        #[cfg(windows)]
        {
            return std::env::var("LOCALAPPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("."))
                .join("Ninimma")
                .join("thumbnails");
        }

        // macOS: ~/Library/Caches/Ninimma/thumbnails
        #[cfg(target_os = "macos")]
        {
            return std::env::var("HOME")
                .map(PathBuf::from)
                .map(|p| {
                    p.join("Library")
                        .join("Caches")
                        .join("Ninimma")
                        .join("thumbnails")
                })
                .unwrap_or_else(|_| PathBuf::from(".cache/ninimma/thumbnails"));
        }

        // Linux: ~/.cache/ninimma/thumbnails (follows XDG)
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            return std::env::var("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".cache")))
                .unwrap_or_else(|_| PathBuf::from(".cache"))
                .join("ninimma")
                .join("thumbnails");
        }

        #[allow(unreachable_code)]
        PathBuf::from(".cache/ninimma/thumbnails")
    }

    /// Call once per frame before any `get` calls.
    pub fn begin_frame(&mut self, ctx: &Context) {
        self.frame += 1;
        if let Ok(mut c) = self.shared.ctx.lock() {
            if c.is_none() {
                *c = Some(ctx.clone());
            }
        }

        let finished = std::mem::take(&mut *self.shared.finished.lock().unwrap());
        // Uploading is cheap at these sizes, but cap it so a burst of results
        // can't stall a single frame.
        let mut remaining = Vec::new();
        for (i, done) in finished.into_iter().enumerate() {
            if i >= 48 {
                remaining.push(done);
                continue;
            }
            // Accept only the newest request for this key: after an
            // invalidate (e.g. a rotate) an older decode may finish last.
            let current = matches!(self.entries.get(&done.key), Some(Entry::Loading(id)) if *id == done.generation);
            if !current {
                continue;
            }
            let texture = done.image.map(|img| {
                ctx.load_texture(
                    format!("thumb:{}:{:?}", done.key.0.display(), done.key.1),
                    img,
                    TextureOptions::LINEAR.with_mipmap_mode(Some(TextureFilter::Linear)),
                )
            });
            self.entries.insert(
                done.key,
                Entry::Ready {
                    texture,
                    last_used: self.frame,
                },
            );
        }
        if !remaining.is_empty() {
            self.shared.finished.lock().unwrap().extend(remaining);
            ctx.request_repaint();
        }

        self.evict(ThumbSize::Icon, MAX_THUMBNAILS);
        self.evict(ThumbSize::Preview, MAX_PREVIEWS);
    }

    fn evict(&mut self, size: ThumbSize, max: usize) {
        let mut ready: Vec<(u64, Key)> = self
            .entries
            .iter()
            .filter_map(|(k, e)| match e {
                Entry::Ready { last_used, .. } if k.1 == size => Some((*last_used, k.clone())),
                _ => None,
            })
            .collect();
        if ready.len() <= max {
            return;
        }
        ready.sort_by_key(|(used, _)| *used);
        let excess = ready.len() - max;
        for (used, key) in ready.into_iter().take(excess) {
            if used == self.frame {
                break; // never evict something drawn this frame
            }
            self.entries.remove(&key);
        }
    }

    /// Thumbnail for an icon/grid view, or `None` while it's being generated
    /// (or if the file has no thumbnail).
    pub fn get(&mut self, path: &Path, size: ThumbSize) -> Option<TextureHandle> {
        let key = (path.to_path_buf(), size);
        match self.entries.get_mut(&key) {
            Some(Entry::Ready { texture, last_used }) => {
                *last_used = self.frame;
                return texture.clone();
            }
            Some(Entry::Loading(_)) => return None,
            None => {}
        }
        self.generation += 1;
        self.entries
            .insert(key.clone(), Entry::Loading(self.generation));
        let mut q = self.shared.queue.lock().unwrap();
        q.jobs.push_back(Job {
            key,
            generation: self.generation,
        });
        while q.jobs.len() > MAX_QUEUE {
            if let Some(old) = q.jobs.pop_front() {
                self.entries.remove(&old.key);
            }
        }
        drop(q);
        self.shared.wake.notify_one();
        None
    }

    /// True while the thumbnail is queued or being generated.
    pub fn is_loading(&self, path: &Path, size: ThumbSize) -> bool {
        matches!(
            self.entries.get(&(path.to_path_buf(), size)),
            Some(Entry::Loading(_))
        )
    }

    /// Whether a thumbnail was ever requested for this file (tests).
    #[cfg(test)]
    pub fn was_requested(&self, path: &Path) -> bool {
        self.entries
            .contains_key(&(path.to_path_buf(), ThumbSize::Icon))
    }

    /// Drop everything for this file so the next request regenerates it
    /// (e.g. after rotating).
    pub fn invalidate(&mut self, path: &Path) {
        for size in [ThumbSize::Icon, ThumbSize::Preview] {
            self.entries.remove(&(path.to_path_buf(), size));
        }
    }

    /// Forget pending work, e.g. when leaving a folder. Loaded textures are
    /// kept (LRU eviction handles memory) so going Back is instant.
    pub fn cancel_pending(&mut self) {
        self.generation += 1;
        let mut q = self.shared.queue.lock().unwrap();
        for job in q.jobs.drain(..) {
            self.entries.remove(&job.key);
        }
        drop(q);
        self.entries.retain(|_, e| !matches!(e, Entry::Loading(_)));
    }

    /// Check if file is a supported image format (can generate thumbnail)
    pub fn is_image_file(path: &Path) -> bool {
        matches!(
            ext(path).as_str(),
            "jpg" | "jpeg" | "jpe" | "jfif" | "png" | "gif" | "bmp" | "ico" | "webp" |
            "tiff" | "tif" | "avif" | "pnm" | "pbm" | "pgm" | "ppm" |
            "dds" | "tga" | "ff" | "farbfeld" | "exr" | "hdr" | "qoi" |
            // Supported via external tools
            "heic" | "heif" | "pdf"
        )
    }

    /// Check if file is a video (can generate thumbnail with ffmpeg)
    pub fn is_video_file(path: &Path) -> bool {
        matches!(
            ext(path).as_str(),
            "mp4"
                | "mkv"
                | "avi"
                | "mov"
                | "wmv"
                | "flv"
                | "webm"
                | "m4v"
                | "mpg"
                | "mpeg"
                | "3gp"
                | "mts"
                | "m2ts"
        )
    }

    pub fn can_generate_thumbnail(path: &Path) -> bool {
        Self::is_image_file(path) || Self::is_video_file(path)
    }

    pub fn is_text_file(path: &Path) -> bool {
        if path.extension().is_some() {
            matches!(
                ext(path).as_str(),
                // Plain text
                "txt" | "md" | "markdown" | "log" | "csv" | "tsv" |
                // Config files
                "json" | "xml" | "yaml" | "yml" | "toml" | "ini" | "cfg" | "conf" | "config" |
                "env" | "properties" | "editorconfig" | "gitignore" | "gitattributes" |
                // Web
                "html" | "htm" | "css" | "scss" | "sass" | "less" | "svg" |
                // JavaScript/TypeScript
                "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" | "vue" | "svelte" |
                // Programming languages
                "rs" | "py" | "pyw" | "rb" | "php" | "java" | "kt" | "kts" | "scala" |
                "go" | "c" | "h" | "cpp" | "hpp" | "cc" | "cxx" | "cs" | "fs" | "fsx" |
                "swift" | "m" | "mm" | "r" | "pl" | "pm" | "lua" | "dart" | "zig" |
                "nim" | "cr" | "ex" | "exs" | "erl" | "hrl" | "clj" | "cljs" | "lisp" |
                "hs" | "elm" | "v" | "vhdl" | "verilog" |
                // Shell/Scripts
                "sh" | "bash" | "zsh" | "fish" | "ps1" | "psm1" | "psd1" | "bat" | "cmd" |
                // Data/Query
                "sql" | "graphql" | "gql" |
                // Documentation
                "rst" | "tex" | "latex" | "adoc" | "asciidoc" |
                // Other
                "dockerfile" | "makefile" | "cmake" | "gradle" | "sbt" |
                "lock" | "sum" | "mod" | "desktop" | "service"
            )
        } else {
            // Check for files without extension that are commonly text
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            matches!(
                name.as_str(),
                "dockerfile"
                    | "makefile"
                    | "gemfile"
                    | "rakefile"
                    | "procfile"
                    | "vagrantfile"
                    | "jenkinsfile"
                    | ".gitignore"
                    | ".gitattributes"
                    | ".editorconfig"
                    | ".env"
                    | ".bashrc"
                    | ".profile"
                    | ".zshrc"
                    | "readme"
                    | "license"
                    | "authors"
                    | "changelog"
                    | "todo"
            )
        }
    }

    /// Get a message about why preview isn't available
    pub fn get_unsupported_message(path: &Path) -> Option<&'static str> {
        match ext(path).as_str() {
            "raw" | "cr2" | "cr3" | "nef" | "arw" | "dng" | "orf" | "rw2" => {
                Some("RAW photo preview not supported")
            }
            "psd" | "ai" | "eps" => Some("Adobe format preview not supported"),
            _ => None,
        }
    }
}

impl Drop for ThumbnailCache {
    fn drop(&mut self) {
        self.shared.queue.lock().unwrap().shutdown = true;
        self.shared.wake.notify_all();
    }
}

fn ext(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn worker_loop(shared: &Shared, cache_dir: &Path) {
    loop {
        let job = {
            let mut q = shared.queue.lock().unwrap();
            loop {
                if q.shutdown {
                    return;
                }
                // Newest first: that's what is on screen now.
                if let Some(job) = q.jobs.pop_back() {
                    break job;
                }
                q = shared.wake.wait(q).unwrap();
            }
        };
        let (path, size) = &job.key;
        // Decoders for untrusted files can panic; never let that kill the worker.
        let image = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            load_or_generate(path, *size, cache_dir)
        }))
        .ok()
        .flatten();
        shared.finished.lock().unwrap().push(Finished {
            key: job.key,
            generation: job.generation,
            image,
        });
        if let Some(ctx) = shared.ctx.lock().unwrap().as_ref() {
            ctx.request_repaint();
        }
    }
}

fn prune_disk_cache(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > DISK_CACHE_MAX_AGE);
        if old {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Cache file name derived from the path plus size and modification time
/// (to the nanosecond), so edits — including our own rotations — are picked up.
fn cache_path(cache_dir: &Path, path: &Path, size: ThumbSize) -> PathBuf {
    // FNV-1a: stable across Rust releases, unlike DefaultHasher.
    let mut hash: u64 = 0xcbf29ce484222325;
    let mut feed = |bytes: &[u8]| {
        for b in bytes {
            hash ^= *b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
    };
    feed(path.to_string_lossy().as_bytes());
    if let Ok(meta) = fs::metadata(path) {
        feed(&meta.len().to_le_bytes());
        if let Ok(d) = meta.modified().and_then(|m| {
            m.duration_since(SystemTime::UNIX_EPOCH)
                .map_err(std::io::Error::other)
        }) {
            feed(&d.as_nanos().to_le_bytes());
        }
    }
    cache_dir.join(format!("{:016x}_{}.png", hash, size.suffix()))
}

pub(crate) fn load_or_generate(
    path: &Path,
    size: ThumbSize,
    cache_dir: &Path,
) -> Option<ColorImage> {
    let max = size.pixels();
    let cached = cache_path(cache_dir, path, size);
    if cached.exists() {
        if let Some(img) = load_scaled(&cached, max) {
            // Touch it so pruning keeps entries that are still in use.
            let _ = fs::File::options()
                .append(true)
                .open(&cached)
                .and_then(|f| f.set_modified(SystemTime::now()));
            return Some(img);
        }
    }

    let ext = ext(path);
    match ext.as_str() {
        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "flv" | "webm" | "m4v" | "mpg" | "mpeg" | "3gp"
        | "mts" | "m2ts" => generate_video_thumbnail(path, &cached, max),
        "heic" | "heif" => generate_heic_thumbnail(path, &cached, max),
        "pdf" => generate_pdf_thumbnail(path, &cached, max),
        _ => {
            let img = decode_oriented(path)?;
            let thumb = img.thumbnail(max, max);
            // Only worth caching if decoding was expensive.
            if img.width() > max || img.height() > max {
                let _ = thumb.save_with_format(&cached, image::ImageFormat::Png);
            }
            Some(to_color_image(&thumb))
        }
    }
}

/// Decode an image and apply its EXIF orientation, so phone photos aren't
/// shown sideways.
pub(crate) fn decode_oriented(path: &Path) -> Option<DynamicImage> {
    if fs::metadata(path).ok()?.len() > SOURCE_SIZE_LIMIT {
        return None;
    }
    let mut reader = ImageReader::open(path).ok()?.with_guessed_format().ok()?;
    // Refuse decompression bombs rather than allocating gigabytes.
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(1024 * 1024 * 1024);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().ok()?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder).ok()?;
    img.apply_orientation(orientation);
    Some(img)
}

fn load_scaled(path: &Path, max: u32) -> Option<ColorImage> {
    let img = decode_oriented(path)?;
    let img = if img.width() > max || img.height() > max {
        img.thumbnail(max, max)
    } else {
        img
    };
    Some(to_color_image(&img))
}

fn to_color_image(img: &DynamicImage) -> ColorImage {
    let rgba = img.to_rgba8();
    ColorImage::from_rgba_unmultiplied(
        [rgba.width() as usize, rgba.height() as usize],
        rgba.as_raw(),
    )
}

fn hidden_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    cmd
}

/// ffprobe, installed alongside ffmpeg.
pub fn find_ffprobe() -> Option<PathBuf> {
    let ffmpeg = find_ffmpeg()?;
    let exe = if cfg!(windows) {
        "ffprobe.exe"
    } else {
        "ffprobe"
    };
    Some(ffmpeg.with_file_name(exe)).filter(|p| p.is_file())
}

/// Find ffmpeg executable (cross-platform)
fn find_ffmpeg() -> Option<PathBuf> {
    FFMPEG_PATH
        .get_or_init(|| {
            let exe = if cfg!(windows) {
                "ffmpeg.exe"
            } else {
                "ffmpeg"
            };
            if let Some(paths) = std::env::var_os("PATH") {
                if let Some(found) = std::env::split_paths(&paths)
                    .map(|d| d.join(exe))
                    .find(|p| p.is_file())
                {
                    return Some(found);
                }
            }

            #[cfg(windows)]
            let candidates: Vec<PathBuf> = {
                let mut v: Vec<PathBuf> = [
                    r"C:\ffmpeg\bin\ffmpeg.exe",
                    r"C:\Program Files\ffmpeg\bin\ffmpeg.exe",
                    r"C:\Program Files (x86)\ffmpeg\bin\ffmpeg.exe",
                ]
                .iter()
                .map(PathBuf::from)
                .collect();
                if let Ok(local) = std::env::var("LOCALAPPDATA") {
                    v.push(
                        PathBuf::from(local)
                            .join("Programs")
                            .join("ffmpeg")
                            .join("bin")
                            .join("ffmpeg.exe"),
                    );
                }
                for program_dir in [r"C:\Program Files", r"C:\Program Files (x86)"] {
                    if let Ok(entries) = fs::read_dir(program_dir) {
                        for entry in entries.flatten() {
                            if entry
                                .file_name()
                                .to_string_lossy()
                                .to_lowercase()
                                .contains("ffmpeg")
                            {
                                v.push(entry.path().join("bin").join("ffmpeg.exe"));
                            }
                        }
                    }
                }
                v
            };
            #[cfg(target_os = "macos")]
            let candidates: Vec<PathBuf> = [
                "/usr/local/bin/ffmpeg",
                "/opt/homebrew/bin/ffmpeg",
                "/opt/local/bin/ffmpeg",
            ]
            .iter()
            .map(PathBuf::from)
            .collect();
            #[cfg(all(unix, not(target_os = "macos")))]
            let candidates: Vec<PathBuf> = [
                "/usr/bin/ffmpeg",
                "/usr/local/bin/ffmpeg",
                "/snap/bin/ffmpeg",
            ]
            .iter()
            .map(PathBuf::from)
            .collect();

            candidates.into_iter().find(|p| p.is_file())
        })
        .clone()
}

/// Whether frames can be taken from videos (ffmpeg is installed).
pub(crate) fn can_read_videos() -> bool {
    find_ffmpeg().is_some()
}

/// A few frames from across a video (at 15%, 50% and 85% of its length;
/// the first second if its length isn't known), for searching what it
/// shows. `work` holds the frames briefly.
pub(crate) fn video_frames(path: &Path, work: &Path) -> Vec<DynamicImage> {
    let Some(ffmpeg) = find_ffmpeg() else {
        return Vec::new();
    };
    let at: Vec<f64> = match crate::properties::video_duration(path) {
        Some(d) if d > 3.0 => vec![d * 0.15, d * 0.5, d * 0.85],
        _ => vec![1.0, 0.0],
    };
    if fs::create_dir_all(work).is_err() {
        return Vec::new();
    }
    let mut frames = Vec::new();
    for (i, seconds) in at.iter().enumerate() {
        let out = work.join(format!("frame-{i}.png"));
        let mut cmd = hidden_command(&ffmpeg);
        cmd.args([
            "-nostdin",
            "-v",
            "error",
            "-y",
            "-ss",
            &format!("{seconds:.2}"),
            "-i",
        ])
        .arg(path)
        .args(["-frames:v", "1", "-vf", "scale=320:-2", "-f", "image2"])
        .arg(&out);
        if cmd.status().is_ok_and(|s| s.success()) {
            if let Some(frame) = image::ImageReader::open(&out)
                .ok()
                .and_then(|r| r.with_guessed_format().ok())
                .and_then(|r| r.decode().ok())
            {
                frames.push(frame);
            }
        }
        let _ = fs::remove_file(&out);
        // A clip too short for the first try: the next one is its first frame.
        if !frames.is_empty() && at.len() == 2 {
            break;
        }
    }
    frames
}

fn generate_video_thumbnail(path: &Path, cache_path: &Path, max_size: u32) -> Option<ColorImage> {
    let ffmpeg = find_ffmpeg()?;
    let input = path.to_string_lossy().to_string();
    let output = cache_path.to_string_lossy().to_string();
    // Fit inside max_size x max_size whatever the aspect ratio.
    let scale = format!(
        "scale='min({max_size},iw)':'min({max_size},ih)':force_original_aspect_ratio=decrease"
    );

    // Seek to 1s (skips black intro frames), then fall back to the first frame
    // for very short clips. `-ss` before `-i` seeks without decoding everything.
    for seek in [Some("00:00:01"), None] {
        let mut cmd = hidden_command(&ffmpeg);
        cmd.args(["-nostdin", "-v", "error", "-y"]);
        if let Some(t) = seek {
            cmd.args(["-ss", t]);
        }
        cmd.args([
            "-i",
            &input,
            "-frames:v",
            "1",
            "-vf",
            &scale,
            "-f",
            "image2",
            &output,
        ]);
        if cmd.status().is_ok_and(|s| s.success()) && cache_path.exists() {
            return load_scaled(cache_path, max_size);
        }
    }
    None
}

fn generate_heic_thumbnail(path: &Path, cache_path: &Path, max_size: u32) -> Option<ColorImage> {
    // macOS: Use built-in 'sips' command (always available, native HEIC support)
    #[cfg(target_os = "macos")]
    {
        let mut cmd = hidden_command("sips");
        cmd.args(["-s", "format", "png", "-Z", &max_size.to_string(), "--out"]);
        cmd.arg(cache_path).arg(path);
        if cmd.status().is_ok_and(|s| s.success()) && cache_path.exists() {
            return load_scaled(cache_path, max_size);
        }
    }

    // Windows: Use PowerShell with the system's HEIF codec (HEVC Extensions)
    #[cfg(windows)]
    {
        let ps_script = format!(
            r#"
Add-Type -AssemblyName System.Drawing
$inputPath = '{}'
$outputPath = '{}'
$maxSize = {}

try {{
    Add-Type -TypeDefinition @"
using System;
using System.Drawing;
using System.Drawing.Imaging;

public class HeicConverter {{
    public static void Convert(string input, string output, int maxSize) {{
        using (var img = Image.FromFile(input)) {{
            int newWidth, newHeight;
            if (img.Width > img.Height) {{
                newWidth = Math.Min(maxSize, img.Width);
                newHeight = (int)(img.Height * ((float)newWidth / img.Width));
            }} else {{
                newHeight = Math.Min(maxSize, img.Height);
                newWidth = (int)(img.Width * ((float)newHeight / img.Height));
            }}
            using (var thumb = img.GetThumbnailImage(newWidth, newHeight, null, IntPtr.Zero)) {{
                thumb.Save(output, ImageFormat.Png);
            }}
        }}
    }}
}}
"@ -ReferencedAssemblies System.Drawing

    [HeicConverter]::Convert($inputPath, $outputPath, $maxSize)
    Write-Output "SUCCESS"
}} catch {{
    Write-Output "FAILED: $_"
}}
"#,
            path.to_string_lossy().replace('\'', "''"),
            cache_path.to_string_lossy().replace('\'', "''"),
            max_size
        );
        let mut cmd = hidden_command("powershell");
        cmd.stdout(std::process::Stdio::piped());
        cmd.args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-WindowStyle",
            "Hidden",
            "-Command",
            &ps_script,
        ]);
        if let Ok(output) = cmd.output() {
            if String::from_utf8_lossy(&output.stdout).contains("SUCCESS") && cache_path.exists() {
                return load_scaled(cache_path, max_size);
            }
        }
    }

    // All platforms: ImageMagick (needs libheif support on Linux)
    generate_with_imagemagick(path, cache_path, max_size)
}

fn imagemagick_programs(windows: bool) -> &'static [&'static str] {
    // Windows' system convert.exe converts filesystems, not images.
    if windows {
        &["magick"]
    } else {
        &["magick", "convert"]
    }
}

fn imagemagick_command(
    program: &str,
    path: &Path,
    cache_path: &Path,
    max_size: u32,
) -> Option<Command> {
    let format = match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "heic" | "heif" => "heic:",
        "pdf" => "pdf:",
        _ => return None,
    };
    // Force the intended reader; a renamed SVG/PS/MVG must not select a delegate.
    let mut input = std::ffi::OsString::from(format);
    input.push(path.canonicalize().ok()?);
    input.push("[0]");
    let mut output = std::ffi::OsString::from("png:");
    output.push(std::path::absolute(cache_path).ok()?);
    let mut cmd = hidden_command(program);
    cmd.args([
        "-limit", "memory", "128MiB", "-limit", "map", "256MiB", "-limit", "disk", "256MiB",
        "-limit", "time", "10", "-limit", "thread", "1",
    ])
    .arg(input)
    .args([
        "-auto-orient",
        "-thumbnail",
        &format!("{max_size}x{max_size}"),
    ]);
    if format == "pdf:" {
        cmd.args(["-background", "white", "-flatten"]);
    }
    cmd.arg(output);
    Some(cmd)
}

fn succeeds_within(mut cmd: Command, limit: Duration) -> bool {
    let Ok(mut child) = cmd.spawn() else {
        return false;
    };
    let deadline = std::time::Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if std::time::Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

fn generate_with_imagemagick(path: &Path, cache_path: &Path, max_size: u32) -> Option<ColorImage> {
    for program in imagemagick_programs(cfg!(windows)) {
        let cmd = imagemagick_command(program, path, cache_path, max_size)?;
        if succeeds_within(cmd, Duration::from_secs(15)) && cache_path.exists() {
            return load_scaled(cache_path, max_size);
        }
    }
    None
}

fn generate_pdf_thumbnail(path: &Path, cache_path: &Path, max_size: u32) -> Option<ColorImage> {
    let input = path.to_string_lossy().to_string();
    let output = cache_path.to_string_lossy().to_string();

    // pdftoppm (poppler-utils) is the most commonly installed on Linux.
    let output_base = cache_path.with_extension("");
    let mut cmd = hidden_command("pdftoppm");
    cmd.args([
        "-png",
        "-singlefile",
        "-f",
        "1",
        "-l",
        "1",
        "-scale-to",
        &max_size.to_string(),
        &input,
    ])
    .arg(&output_base);
    if cmd.status().is_ok_and(|s| s.success()) {
        let produced = output_base.with_extension("png");
        if produced.exists() {
            if produced != cache_path {
                let _ = fs::rename(&produced, cache_path);
            }
            return load_scaled(cache_path, max_size);
        }
    }

    // mutool (MuPDF)
    let mut cmd = hidden_command("mutool");
    cmd.args([
        "convert",
        "-o",
        &output,
        "-O",
        &format!("width={max_size}"),
        &input,
        "1",
    ]);
    if cmd.status().is_ok_and(|s| s.success()) && cache_path.exists() {
        return load_scaled(cache_path, max_size);
    }

    // The explicit PDF reader may use Ghostscript; never infer another format.
    generate_with_imagemagick(path, cache_path, max_size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imagemagick_forces_only_expected_readers_and_bounded_resources() {
        let dir = tempfile::tempdir().unwrap();
        for (name, contents, prefix) in [
            ("renamed.heic", "%!PS-Adobe-3.0", "heic:"),
            (
                "-renamed.HEIF",
                "<svg xmlns='http://www.w3.org/2000/svg'/>",
                "heic:",
            ),
            ("drawing.heic", "push graphic-context", "heic:"),
            ("page.pdf", "%PDF-1.7", "pdf:"),
        ] {
            let source = dir.path().join(name);
            fs::write(&source, contents).unwrap();
            let output = dir.path().join("thumbnail.png");
            let command = imagemagick_command("magick", &source, &output, 256).unwrap();
            let args: Vec<_> = command.get_args().map(|a| a.to_os_string()).collect();
            let mut expected = std::ffi::OsString::from(prefix);
            expected.push(source.canonicalize().unwrap());
            expected.push("[0]");
            let input = args.iter().position(|arg| arg == &expected).unwrap();
            assert_eq!(input, 15, "limits must precede reading the input");
            assert_eq!(
                args[..input],
                [
                    "-limit", "memory", "128MiB", "-limit", "map", "256MiB", "-limit", "disk",
                    "256MiB", "-limit", "time", "10", "-limit", "thread", "1"
                ]
                .map(std::ffi::OsString::from)
            );
            assert!(args.last().unwrap().to_string_lossy().starts_with("png:"));
            assert_eq!(args.iter().any(|a| a == "-flatten"), prefix == "pdf:");
        }
        for ext in ["svg", "mvg", "ps", "png", "jpg"] {
            let path = dir.path().join(format!("unneeded.{ext}"));
            fs::write(&path, b"fixture").unwrap();
            assert!(
                imagemagick_command("magick", &path, &dir.path().join("out.png"), 256).is_none()
            );
        }
    }

    #[test]
    fn windows_never_uses_the_filesystem_convert_tool() {
        assert_eq!(imagemagick_programs(true), &["magick"]);
        assert_eq!(imagemagick_programs(false), &["magick", "convert"]);
    }

    #[cfg(unix)]
    #[test]
    fn image_converter_deadline_kills_and_reaps_the_process() {
        let mut slow = hidden_command("sleep");
        slow.arg("1");
        assert!(!succeeds_within(slow, Duration::from_millis(20)));
        let mut success = hidden_command("sh");
        success.args(["-c", "exit 0"]);
        assert!(succeeds_within(success, Duration::from_secs(5)));
        let mut failure = hidden_command("sh");
        failure.args(["-c", "exit 1"]);
        assert!(!succeeds_within(failure, Duration::from_secs(5)));
    }

    #[test]
    fn cache_key_changes_when_file_is_modified() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.png");
        fs::write(&f, b"one").unwrap();
        let before = cache_path(dir.path(), &f, ThumbSize::Icon);
        // Same length, same second: only a sub-second difference, as with a
        // quick rotate. (1 µs: NTFS stores times in 100 ns steps.)
        let t = fs::metadata(&f).unwrap().modified().unwrap();
        fs::write(&f, b"two").unwrap();
        fs::File::options()
            .write(true)
            .open(&f)
            .unwrap()
            .set_modified(t + Duration::from_micros(1))
            .unwrap();
        assert_ne!(before, cache_path(dir.path(), &f, ThumbSize::Icon));
        assert_ne!(
            cache_path(dir.path(), &f, ThumbSize::Icon),
            cache_path(dir.path(), &f, ThumbSize::Preview)
        );
    }

    #[test]
    fn thumbnails_respect_exif_orientation() {
        let dir = tempfile::tempdir().unwrap();
        let jpg = dir.path().join("p.jpg");
        image::DynamicImage::new_rgb8(400, 100).save(&jpg).unwrap();
        crate::image_ops::rotate_file(&jpg, crate::image_ops::Rotation::Clockwise).unwrap();
        let thumb = load_or_generate(&jpg, ThumbSize::Icon, dir.path()).unwrap();
        assert_eq!(thumb.size, [64, 256]); // portrait now
    }

    #[test]
    fn stale_result_after_invalidate_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let mut cache = ThumbnailCache::with_cache_dir(dir.path().into());
        let ctx = Context::default();
        let path = dir.path().join("missing.png");
        let key = (path.clone(), ThumbSize::Icon);
        assert!(cache.get(&path, ThumbSize::Icon).is_none());
        let old_id = cache.generation;
        cache.invalidate(&path);
        assert!(cache.get(&path, ThumbSize::Icon).is_none());
        let new_id = cache.generation;
        // Let the workers answer both real requests, then discard those
        // answers and deliver results in a controlled order instead.
        for _ in 0..500 {
            if cache.shared.finished.lock().unwrap().len() == 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        cache.shared.finished.lock().unwrap().clear();

        let img = |c: u8| Some(ColorImage::from_rgba_unmultiplied([1, 1], &[c, c, c, 255]));
        // The newer result arrives first, then the stale one.
        cache.shared.finished.lock().unwrap().push(Finished {
            key: key.clone(),
            generation: new_id,
            image: img(200),
        });
        cache.begin_frame(&ctx);
        let fresh = cache
            .get(&path, ThumbSize::Icon)
            .expect("newest result shown")
            .id();
        cache.shared.finished.lock().unwrap().push(Finished {
            key: key.clone(),
            generation: old_id,
            image: img(10),
        });
        cache.begin_frame(&ctx);
        // The stale result must not replace the fresh texture.
        assert_eq!(cache.get(&path, ThumbSize::Icon).unwrap().id(), fresh);
    }

    #[test]
    fn corrupt_images_yield_none_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("bad.jpg");
        fs::write(&f, b"\xFF\xD8\xFF\xE0garbage").unwrap();
        assert!(load_or_generate(&f, ThumbSize::Icon, dir.path()).is_none());
    }
}
