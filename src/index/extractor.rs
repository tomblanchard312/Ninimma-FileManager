// Ninimma modification: independent project branding and integration.
//! Reading text and photo details out of files, for the index.
//!
//! Everything here treats files as untrusted. Each reader is bounded: file
//! size, unpacked size of Office parts, number of cells or slides, number
//! of characters kept, and time spent. Nothing is executed: no macros,
//! scripts, embedded objects or links. PDF text comes from Poppler's
//! `pdftotext`, run with fixed arguments and no shell, killed if it runs
//! too long. Decoder panics are caught, and failures recorded as a short
//! category, never as file contents.

use crate::preview::{spreadsheet_is_safe_to_open, xml_text_runs, MAX_XML_PART};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Recorded per file in `files.extraction_state`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i64)]
pub enum State {
    /// Set by SQL when a file is new or changed (`extraction_state = 0`).
    #[allow(dead_code)]
    Pending = 0,
    Done = 1,
    /// Not a format we read (only the name and details are indexed).
    Unsupported = 2,
    Failed = 3,
    TooLarge = 4,
    /// A format we read, but its reader (pdftotext) isn't installed. Tried
    /// again once it is.
    NeedsTool = 5,
}

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Larger text files aren't read (spec default: 2 MiB).
    pub max_text_file: u64,
    /// Larger Office and PDF files aren't read.
    pub max_document_file: u64,
    /// Text kept per file.
    pub max_chars: usize,
    pub time: Duration,
}

impl Limits {
    pub fn new(max_text_file: u64) -> Self {
        Self {
            max_text_file,
            max_document_file: 50 * 1024 * 1024,
            max_chars: 200_000,
            time: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, Default, PartialEq)]
pub struct Extracted {
    pub text: String,
    /// When a photo was taken (EXIF), as ns since the epoch, local time.
    pub taken_ns: Option<i64>,
    /// GPS position (EXIF).
    pub gps: Option<(f64, f64)>,
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Read(Extracted),
    Skipped(State),
    /// Failed; the text is a short category ("unreadable zip"), no content.
    Failed(&'static str),
}

const PHOTO_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "jpe", "jfif", "tif", "tiff", "heic", "heif", "png", "webp", "dng",
];

fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

pub fn extract(path: &Path, size: u64, limits: &Limits) -> Outcome {
    let ext = extension(path);
    let path = path.to_path_buf();
    let limits = *limits;
    // Third-party decoders get untrusted input; a panic means "failed".
    std::panic::catch_unwind(move || extract_inner(&path, &ext, size, &limits))
        .unwrap_or(Outcome::Failed("decoder crashed"))
}

fn extract_inner(path: &Path, ext: &str, size: u64, limits: &Limits) -> Outcome {
    let deadline = Instant::now() + limits.time;
    let document = |reader: fn(&Path, &Limits, Instant) -> Result<String, &'static str>| {
        if size > limits.max_document_file {
            return Outcome::Skipped(State::TooLarge);
        }
        match reader(path, limits, deadline) {
            Ok(text) => Outcome::Read(Extracted {
                text: bounded(text, limits.max_chars),
                ..Default::default()
            }),
            Err("needs pdftotext") => Outcome::Skipped(State::NeedsTool),
            Err(why) => Outcome::Failed(why),
        }
    };
    match ext {
        e if PHOTO_EXTENSIONS.contains(&e) => Outcome::Read(photo_details(path)),
        "docx" => document(read_docx),
        "pptx" => document(read_pptx),
        "xlsx" | "xlsm" | "xls" | "ods" => document(read_spreadsheet),
        "pdf" => document(read_pdf),
        _ if crate::thumbnail::ThumbnailCache::is_text_file(path) => {
            if size > limits.max_text_file {
                return Outcome::Skipped(State::TooLarge);
            }
            match read_text(path, limits) {
                Ok(text) => Outcome::Read(Extracted {
                    text,
                    ..Default::default()
                }),
                Err(why) => Outcome::Failed(why),
            }
        }
        _ => Outcome::Skipped(State::Unsupported),
    }
}

/// Keeps at most `max` characters.
fn bounded(mut text: String, max: usize) -> String {
    if let Some((cut, _)) = text.char_indices().nth(max) {
        text.truncate(cut);
    }
    text
}

fn read_text(path: &Path, limits: &Limits) -> Result<String, &'static str> {
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|f| f.take(limits.max_text_file).read_to_end(&mut bytes))
        .map_err(|_| "unreadable")?;
    let text = if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        let le = bytes[0] == 0xFF;
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| {
                if le {
                    u16::from_le_bytes(*c)
                } else {
                    u16::from_be_bytes(*c)
                }
            })
            .collect();
        String::from_utf16_lossy(&units)
    } else if bytes.contains(&0) {
        return Err("binary");
    } else {
        String::from_utf8_lossy(bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes))
            .into_owned()
    };
    let text = match extension(path).as_str() {
        "html" | "htm" | "xml" | "svg" => strip_tags(&text),
        _ => text,
    };
    Ok(bounded(text, limits.max_chars))
}

/// Text between tags, without scripts and styles. Only for indexing, so
/// it doesn't need to be a full HTML parser.
fn strip_tags(markup: &str) -> String {
    let mut out = String::with_capacity(markup.len() / 2);
    let mut rest = markup;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        rest = &rest[open..];
        let lower: String = rest.chars().take(8).collect::<String>().to_lowercase();
        let skip_to = if lower.starts_with("<script") {
            Some("</script")
        } else if lower.starts_with("<style") {
            Some("</style")
        } else {
            None
        };
        if let Some(end_tag) = skip_to {
            match rest.to_lowercase().find(end_tag) {
                Some(i) => rest = &rest[i..],
                None => return out,
            }
        }
        match rest.find('>') {
            Some(close) => {
                out.push(' ');
                rest = &rest[close + 1..];
            }
            None => return out,
        }
    }
    out.push_str(rest);
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
}

/// Opens a zip-based Office file and reads one XML part, refusing parts
/// whose declared unpacked size is too large (zip bombs).
fn read_zip_part(
    archive: &mut zip::ZipArchive<BufReader<File>>,
    name: &str,
) -> Result<String, &'static str> {
    let part = archive.by_name(name).map_err(|_| "missing part")?;
    if part.size() > MAX_XML_PART {
        return Err("part too large");
    }
    let mut xml = String::new();
    part.take(MAX_XML_PART)
        .read_to_string(&mut xml)
        .map_err(|_| "unreadable part")?;
    Ok(xml)
}

fn open_zip(path: &Path) -> Result<zip::ZipArchive<BufReader<File>>, &'static str> {
    let file = File::open(path).map_err(|_| "unreadable")?;
    zip::ZipArchive::new(BufReader::new(file)).map_err(|_| "not a valid document")
}

fn read_docx(path: &Path, _limits: &Limits, _deadline: Instant) -> Result<String, &'static str> {
    let mut archive = open_zip(path)?;
    let xml = read_zip_part(&mut archive, "word/document.xml")?;
    Ok(xml_text_runs(&xml, b"w:t", Some(b"w:p")))
}

fn read_pptx(path: &Path, limits: &Limits, deadline: Instant) -> Result<String, &'static str> {
    let mut archive = open_zip(path)?;
    let mut slides: Vec<(u32, String)> = archive
        .file_names()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "unreadable zip")?
        .into_iter()
        .filter_map(|n| {
            Some((
                n.strip_prefix("ppt/slides/slide")?
                    .strip_suffix(".xml")?
                    .parse()
                    .ok()?,
                n.to_string(),
            ))
        })
        .collect();
    slides.sort();
    let mut text = String::new();
    for (_, name) in slides.iter().take(500) {
        if Instant::now() > deadline || text.len() > limits.max_chars * 4 {
            break;
        }
        let xml = read_zip_part(&mut archive, name)?;
        text.push_str(&xml_text_runs(&xml, b"a:t", Some(b"a:p")));
        text.push('\n');
    }
    Ok(text)
}

fn read_spreadsheet(
    path: &Path,
    limits: &Limits,
    deadline: Instant,
) -> Result<String, &'static str> {
    use calamine::{open_workbook_auto, Data, Reader};
    if !spreadsheet_is_safe_to_open(path) {
        return Err("too large when unpacked");
    }
    let mut workbook = open_workbook_auto(path).map_err(|_| "not a valid spreadsheet")?;
    let mut text = String::new();
    let mut cells = 0usize;
    for sheet in workbook.sheet_names().into_iter().take(50) {
        text.push_str(&sheet);
        text.push('\n');
        let Ok(range) = workbook.worksheet_range(&sheet) else {
            continue;
        };
        for row in range.rows() {
            for cell in row {
                cells += 1;
                match cell {
                    Data::String(s) => text.push_str(s),
                    Data::Float(f) => text.push_str(&f.to_string()),
                    Data::Int(i) => text.push_str(&i.to_string()),
                    Data::DateTime(d) => text.push_str(&d.to_string()),
                    _ => continue,
                }
                text.push(' ');
            }
            text.push('\n');
            if cells > 200_000 || text.len() > limits.max_chars * 4 || Instant::now() > deadline {
                return Ok(text);
            }
        }
    }
    Ok(text)
}

/// Poppler's `pdftotext`, when installed. Without it, PDFs are indexed by
/// name only (and tried again after it's installed).
fn read_pdf(path: &Path, limits: &Limits, deadline: Instant) -> Result<String, &'static str> {
    let Some(tool) = pdftotext() else {
        return Err("needs pdftotext");
    };
    let mut child = Command::new(tool)
        // At most 100 pages, UTF-8, to stdout. The path is one argument;
        // there's no shell to interpret it.
        .args(["-q", "-l", "100", "-enc", "UTF-8"])
        .arg(path)
        .arg("-")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "pdftotext failed to start")?;
    let mut stdout = child.stdout.take().expect("piped");
    let cap = (limits.max_chars * 4) as u64;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = (&mut stdout).take(cap).read_to_end(&mut out);
        out
    });
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("timed out");
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => return Err("pdftotext failed"),
        }
    }
    let bytes = reader.join().map_err(|_| "pdftotext failed")?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn pdftotext() -> Option<&'static Path> {
    static TOOL: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    TOOL.get_or_init(|| {
        let name = if cfg!(windows) {
            "pdftotext.exe"
        } else {
            "pdftotext"
        };
        std::env::var_os("PATH")
            .into_iter()
            .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
            .map(|dir| dir.join(name))
            .find(|p| p.is_file())
    })
    .as_deref()
}

pub fn pdf_reader_available() -> bool {
    pdftotext().is_some()
}

/// When and where a photo was taken, from its EXIF data.
fn photo_details(path: &Path) -> Extracted {
    use exif::{In, Reader, Tag, Value};
    let mut details = Extracted::default();
    let Ok(file) = File::open(path) else {
        return details;
    };
    let Ok(exif) = Reader::new().read_from_container(&mut BufReader::new(file)) else {
        return details;
    };
    let ascii = |tag| match exif.get_field(tag, In::PRIMARY).map(|f| &f.value) {
        Some(Value::Ascii(v)) => v.first().cloned(),
        _ => None,
    };
    if let Some(raw) = ascii(Tag::DateTimeOriginal).or_else(|| ascii(Tag::DateTime)) {
        if let Ok(dt) = exif::DateTime::from_ascii(&raw) {
            details.taken_ns =
                chrono::NaiveDate::from_ymd_opt(dt.year as i32, dt.month as u32, dt.day as u32)
                    .and_then(|d| d.and_hms_opt(dt.hour as u32, dt.minute as u32, dt.second as u32))
                    .and_then(|t| t.and_local_timezone(chrono::Local).earliest())
                    .and_then(|t| t.timestamp_nanos_opt());
        }
    }
    let coordinate = |value_tag, ref_tag, negative: u8| -> Option<f64> {
        let Some(Value::Rational(parts)) = exif.get_field(value_tag, In::PRIMARY).map(|f| &f.value)
        else {
            return None;
        };
        if parts.len() < 3 || parts.iter().any(|r| r.denom == 0) {
            return None;
        }
        let degrees = parts[0].to_f64() + parts[1].to_f64() / 60.0 + parts[2].to_f64() / 3600.0;
        let sign = match ascii(ref_tag) {
            Some(r) if r.first() == Some(&negative) => -1.0,
            _ => 1.0,
        };
        Some(sign * degrees)
    };
    if let (Some(lat), Some(lon)) = (
        coordinate(Tag::GPSLatitude, Tag::GPSLatitudeRef, b'S'),
        coordinate(Tag::GPSLongitude, Tag::GPSLongitudeRef, b'W'),
    ) {
        details.gps = Some((lat, lon));
    }
    details
}

/// Up to three representative pieces of a document, for meaning-based
/// search later: the beginning, then evenly spaced pieces of the rest.
pub fn chunks(text: &str) -> Vec<(usize, usize, String)> {
    const CHUNK: usize = 1000;
    // Each piece costs ~150 ms to embed on one thread, so three is the
    // default (the spec allows three to five).
    const MAX_CHUNKS: usize = 3;
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    if chars.iter().all(|(_, c)| c.is_whitespace()) {
        return Vec::new();
    }
    let n = chars.len();
    let piece = |start: usize| {
        let end = (start + CHUNK).min(n);
        let from = chars[start].0;
        let to = if end == n { text.len() } else { chars[end].0 };
        (from, to, text[from..to].trim().to_string())
    };
    let mut out = vec![piece(0)];
    if n > CHUNK {
        // The rest is sampled evenly: starts from CHUNK to n - CHUNK.
        let extra = (n - CHUNK).div_ceil(CHUNK).min(MAX_CHUNKS - 1);
        let span = n.saturating_sub(2 * CHUNK);
        for i in 0..extra {
            let start = CHUNK + span * i / (extra - 1).max(1);
            out.push(piece(start));
        }
    }
    out.retain(|(_, _, t)| !t.is_empty());
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    fn limits() -> Limits {
        Limits::new(2 * 1024 * 1024)
    }

    fn text_of(outcome: Outcome) -> String {
        match outcome {
            Outcome::Read(e) => e.text,
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn plain_text_html_and_markdown() {
        let dir = tempfile::tempdir().unwrap();
        let md = dir.path().join("notes.md");
        fs::write(&md, "# Arizona trip\nHotel receipt total $120").unwrap();
        assert!(text_of(extract(&md, 40, &limits())).contains("Hotel receipt"));
        let html = dir.path().join("page.html");
        fs::write(&html, "<html><style>p{color:red}</style><script>alert('x')</script><p>Fish &amp; chips</p></html>").unwrap();
        let text = text_of(extract(&html, 100, &limits()));
        assert!(text.contains("Fish & chips"));
        assert!(!text.contains("alert") && !text.contains("color"));
    }

    #[test]
    fn limits_hold_for_large_and_binary_files() {
        let dir = tempfile::tempdir().unwrap();
        let big = dir.path().join("big.txt");
        fs::write(&big, "a".repeat(100)).unwrap();
        let small = Limits {
            max_text_file: 50,
            ..limits()
        };
        assert_eq!(
            extract(&big, 100, &small),
            Outcome::Skipped(State::TooLarge)
        );
        let few = Limits {
            max_chars: 10,
            ..limits()
        };
        assert_eq!(text_of(extract(&big, 100, &few)).chars().count(), 10);
        let bin = dir.path().join("x.txt");
        fs::write(&bin, b"ab\0cd").unwrap();
        assert_eq!(extract(&bin, 5, &limits()), Outcome::Failed("binary"));
        let unknown = dir.path().join("x.bin");
        fs::write(&unknown, b"data").unwrap();
        assert_eq!(
            extract(&unknown, 4, &limits()),
            Outcome::Skipped(State::Unsupported)
        );
    }

    #[test]
    fn presentations_are_read_slide_by_slide_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let pptx = dir.path().join("roadmap.pptx");
        let mut zip = zip::ZipWriter::new(fs::File::create(&pptx).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        // Stored out of order; slide 10 must come after slide 2.
        for (name, text) in [
            ("ppt/slides/slide10.xml", "Hiring plan"),
            ("ppt/slides/slide2.xml", "Quarterly roadmap"),
            ("ppt/slideLayouts/slideLayout1.xml", "not a slide"),
        ] {
            zip.start_file(name, options).unwrap();
            zip.write_all(
                format!("<p:sld><a:p><a:r><a:t>{text}</a:t></a:r></a:p></p:sld>").as_bytes(),
            )
            .unwrap();
        }
        zip.finish().unwrap();
        let size = fs::metadata(&pptx).unwrap().len();
        let text = text_of(extract(&pptx, size, &limits()));
        let (roadmap, hiring) = (text.find("Quarterly roadmap"), text.find("Hiring plan"));
        assert!(roadmap.is_some() && hiring.is_some(), "{text:?}");
        assert!(roadmap < hiring, "slides in order: {text:?}");
        assert!(!text.contains("not a slide"), "{text:?}");
        // The preview reads them too.
        let preview = crate::preview::extract_pptx(&pptx).unwrap();
        assert!(
            preview.contains("Quarterly roadmap") && preview.contains("Hiring plan"),
            "{preview:?}"
        );
    }

    #[test]
    fn word_documents_and_malformed_zips() {
        let dir = tempfile::tempdir().unwrap();
        let docx = dir.path().join("receipt.docx");
        let mut zip = zip::ZipWriter::new(fs::File::create(&docx).unwrap());
        zip.start_file(
            "word/document.xml",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(br#"<w:document><w:body><w:p><w:r><w:t>Grand Canyon Lodge</w:t></w:r></w:p></w:body></w:document>"#).unwrap();
        zip.finish().unwrap();
        let size = fs::metadata(&docx).unwrap().len();
        assert!(text_of(extract(&docx, size, &limits())).contains("Grand Canyon Lodge"));

        let broken = dir.path().join("broken.docx");
        fs::write(&broken, b"PK\x03\x04 not really a zip").unwrap();
        assert!(matches!(
            extract(&broken, 20, &limits()),
            Outcome::Failed(_)
        ));
    }

    #[test]
    fn office_zip_bombs_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let docx = dir.path().join("bomb.docx");
        let mut zip = zip::ZipWriter::new(fs::File::create(&docx).unwrap());
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("word/document.xml", opts).unwrap();
        let chunk = vec![b' '; 1024 * 1024];
        for _ in 0..(MAX_XML_PART / chunk.len() as u64 + 2) {
            zip.write_all(&chunk).unwrap();
        }
        zip.finish().unwrap();
        let size = fs::metadata(&docx).unwrap().len();
        assert_eq!(
            extract(&docx, size, &limits()),
            Outcome::Failed("part too large")
        );
    }

    #[test]
    fn pdf_text_when_pdftotext_is_installed() {
        if !pdf_reader_available() {
            return;
        }
        // A minimal one-page PDF saying "Receipt Phoenix".
        let pdf = b"%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 300 100]/Contents 4 0 R/Resources<</Font<</F1 5 0 R>>>>>>endobj\n4 0 obj<</Length 44>>stream\nBT /F1 12 Tf 20 50 Td (Receipt Phoenix) Tj ET\nendstream endobj\n5 0 obj<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>endobj\ntrailer<</Root 1 0 R>>\n%%EOF\n";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.pdf");
        fs::write(&path, pdf).unwrap();
        assert!(text_of(extract(&path, pdf.len() as u64, &limits())).contains("Receipt Phoenix"));
    }

    #[test]
    fn photos_give_date_taken_and_position() {
        // A JPEG with EXIF: taken 2025-12-20 14:30:00 at 34°52'11"N 111°45'40"W (Sedona).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("IMG_0001.jpg");
        fs::write(&path, test_jpeg_with_exif()).unwrap();
        let Outcome::Read(details) = extract(&path, 0, &limits()) else {
            panic!()
        };
        let taken = chrono::DateTime::from_timestamp_nanos(details.taken_ns.unwrap())
            .with_timezone(&chrono::Local);
        assert_eq!(
            taken.format("%Y-%m-%d %H:%M").to_string(),
            "2025-12-20 14:30"
        );
        let (lat, lon) = details.gps.unwrap();
        assert!(
            (lat - 34.8697).abs() < 0.001 && (lon + 111.7611).abs() < 0.001,
            "{lat},{lon}"
        );
    }

    /// A tiny JPEG carrying an EXIF block with DateTimeOriginal and GPS.
    pub(crate) fn test_jpeg_with_exif() -> Vec<u8> {
        use exif::experimental::Writer;
        use exif::{Field, In, Rational, Tag, Value};
        let date = Field {
            tag: Tag::DateTimeOriginal,
            ifd_num: In::PRIMARY,
            value: Value::Ascii(vec![b"2025:12:20 14:30:00".to_vec()]),
        };
        let lat_ref = Field {
            tag: Tag::GPSLatitudeRef,
            ifd_num: In::PRIMARY,
            value: Value::Ascii(vec![b"N".to_vec()]),
        };
        let lat = Field {
            tag: Tag::GPSLatitude,
            ifd_num: In::PRIMARY,
            value: Value::Rational(vec![
                Rational::from((34, 1)),
                Rational::from((52, 1)),
                Rational::from((11, 1)),
            ]),
        };
        let lon_ref = Field {
            tag: Tag::GPSLongitudeRef,
            ifd_num: In::PRIMARY,
            value: Value::Ascii(vec![b"W".to_vec()]),
        };
        let lon = Field {
            tag: Tag::GPSLongitude,
            ifd_num: In::PRIMARY,
            value: Value::Rational(vec![
                Rational::from((111, 1)),
                Rational::from((45, 1)),
                Rational::from((40, 1)),
            ]),
        };
        let mut writer = Writer::new();
        for f in [&date, &lat_ref, &lat, &lon_ref, &lon] {
            writer.push_field(f);
        }
        let mut tiff = std::io::Cursor::new(Vec::new());
        writer.write(&mut tiff, false).unwrap();
        let tiff = tiff.into_inner();

        let mut jpeg = Vec::new();
        image::DynamicImage::new_rgb8(4, 4)
            .write_to(
                &mut std::io::Cursor::new(&mut jpeg),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        // Insert an APP1 "Exif" segment right after the SOI marker.
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend_from_slice(&tiff);
        let mut out = jpeg[..2].to_vec();
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&app1);
        out.extend_from_slice(&jpeg[2..]);
        out
    }

    #[test]
    fn chunks_are_few_and_cover_the_document() {
        assert!(chunks("   ").is_empty());
        let short = chunks("A hotel receipt.");
        assert_eq!(short.len(), 1);
        let long: String = (0..20_000)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect();
        let pieces = chunks(&long);
        assert!(pieces.len() <= 3 && pieces.len() >= 2, "{}", pieces.len());
        assert_eq!(pieces[0].0, 0);
        assert!(pieces
            .iter()
            .all(|(s, e, t)| e > s && t.chars().count() <= 1000));
    }
}
