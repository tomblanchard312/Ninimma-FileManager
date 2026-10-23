// Ninimma modification: independent project branding and integration.
//! Text extraction for the preview pane: plain text, Excel, Word and PowerPoint.

use calamine::{open_workbook_auto, Data, Reader};
use quick_xml::events::Event;
use quick_xml::Reader as XmlReader;
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::Path;

use crate::thumbnail::ThumbnailCache;

const TEXT_PREVIEW_BYTES: u64 = 512 * 1024;
const TEXT_PREVIEW_CHARS: usize = 3000;
/// Caps on what an Office preview may read, so a huge file or a ZIP bomb
/// can't exhaust memory just by being selected.
pub(crate) const MAX_XML_PART: u64 = 4 * 1024 * 1024;
const MAX_SPREADSHEET_FILE: u64 = 50 * 1024 * 1024;
const MAX_SPREADSHEET_UNPACKED: u64 = 200 * 1024 * 1024;

/// The spreadsheet reader loads whole sheets into memory, so check the file
/// size and, for zip-based formats, the total declared unpacked size first.
pub(crate) fn spreadsheet_is_safe_to_open(path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    if meta.len() > MAX_SPREADSHEET_FILE {
        return false;
    }
    let Ok(file) = File::open(path) else {
        return false;
    };
    match zip::ZipArchive::new(BufReader::new(file)) {
        Ok(mut zip) => {
            let mut total: u64 = 0;
            for i in 0..zip.len() {
                match zip.by_index_raw(i) {
                    Ok(entry) => total = total.saturating_add(entry.size()),
                    Err(_) => return false,
                }
            }
            total <= MAX_SPREADSHEET_UNPACKED
        }
        Err(_) => true, // legacy .xls: bounded by the file size check
    }
}

pub fn is_office_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .as_deref(),
        Some(
            "doc"
                | "docx"
                | "xls"
                | "xlsx"
                | "xlsm"
                | "xlsb"
                | "ppt"
                | "pptx"
                | "odt"
                | "ods"
                | "odp"
        )
    )
}

/// Returns preview text, or a short explanation if there's nothing to show.
pub fn load_preview_text(path: &Path) -> String {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let text = match ext.as_str() {
        "xlsx" | "xls" | "xlsm" | "xlsb" | "ods" => extract_spreadsheet(path),
        "docx" => extract_office_xml(
            path,
            "📝 Word Document",
            |n| n == "word/document.xml",
            b"w:t",
            Some(b"w:p"),
        ),
        "pptx" => extract_pptx(path),
        _ if ThumbnailCache::is_text_file(path) => load_text(path),
        _ => None,
    };
    text.unwrap_or_else(|| "No preview available".to_string())
}

fn load_text(path: &Path) -> Option<String> {
    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take(TEXT_PREVIEW_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    let truncated_file = fs::metadata(path)
        .map(|m| m.len() > TEXT_PREVIEW_BYTES)
        .unwrap_or(false);

    let content = if bytes.starts_with(&[0xFF, 0xFE]) {
        decode_utf16(&bytes[2..], u16::from_le_bytes)
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        decode_utf16(&bytes[2..], u16::from_be_bytes)
    } else if bytes.contains(&0) {
        return Some("[Binary file]".to_string());
    } else {
        String::from_utf8_lossy(bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes))
            .into_owned()
    };

    let mut chars = content.chars();
    let mut preview: String = chars.by_ref().take(TEXT_PREVIEW_CHARS).collect();
    if chars.next().is_some() || truncated_file {
        preview.push_str("...\n\n[File truncated for preview]");
    }
    Some(preview)
}

fn decode_utf16(bytes: &[u8], from: fn([u8; 2]) -> u16) -> String {
    let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|c| from(*c)).collect();
    String::from_utf16_lossy(&units)
}

fn extract_spreadsheet(path: &Path) -> Option<String> {
    if !spreadsheet_is_safe_to_open(path) {
        return Some("[Spreadsheet too large to preview]".to_string());
    }
    let mut workbook = open_workbook_auto(path).ok()?;
    let mut preview = String::new();

    for (idx, sheet_name) in workbook.sheet_names().iter().enumerate().take(3) {
        if idx > 0 {
            preview.push_str("\n\n");
        }
        preview.push_str(&format!("📊 Sheet: {sheet_name}\n{}\n", "─".repeat(30)));

        if let Ok(range) = workbook.worksheet_range(sheet_name) {
            let mut row_count = 0;
            for row in range.rows().take(40) {
                let row_text: Vec<String> = row
                    .iter()
                    .take(6)
                    .map(|cell| match cell {
                        Data::Empty => String::new(),
                        Data::String(s) => s.clone(),
                        Data::Float(f) => format!("{f}"),
                        Data::Int(i) => i.to_string(),
                        Data::Bool(b) => b.to_string(),
                        Data::Error(e) => format!("#{e:?}"),
                        Data::DateTime(dt) => dt.to_string(),
                        other => other.to_string(),
                    })
                    .collect();

                if row_text.iter().any(|s| !s.is_empty()) {
                    preview.push_str(&row_text.join(" | "));
                    preview.push('\n');
                    row_count += 1;
                }
                if row_count >= 15 {
                    preview.push_str("...\n");
                    break;
                }
            }
        }
    }

    (!preview.is_empty()).then_some(preview)
}

pub(crate) fn extract_pptx(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let mut archive = zip::ZipArchive::new(BufReader::new(file)).ok()?;

    let mut slides: Vec<(u32, String)> = archive
        .file_names()
        .filter_map(|n| {
            let num = n
                .strip_prefix("ppt/slides/slide")?
                .strip_suffix(".xml")?
                .parse()
                .ok()?;
            Some((num, n.to_string()))
        })
        .collect();
    slides.sort();

    let mut preview = format!("📽️ PowerPoint Presentation\n{}\n", "─".repeat(30));
    for (num, name) in slides.iter().take(10) {
        preview.push_str(&format!("\n📄 Slide {num}\n"));
        let mut xml = String::new();
        let read = archive
            .by_name(name)
            .map(|f| f.take(MAX_XML_PART).read_to_string(&mut xml));
        if matches!(read, Ok(Ok(_))) {
            for line in xml_text_runs(&xml, b"a:t", Some(b"a:p"))
                .lines()
                .filter(|l| !l.trim().is_empty())
            {
                preview.push_str("  • ");
                preview.push_str(line);
                preview.push('\n');
            }
        }
        if preview.chars().count() > 2000 {
            preview.push_str("\n[Presentation truncated for preview]");
            break;
        }
    }
    (!slides.is_empty()).then_some(preview)
}

fn extract_office_xml(
    path: &Path,
    title: &str,
    part: impl Fn(&str) -> bool,
    text_tag: &[u8],
    para_tag: Option<&[u8]>,
) -> Option<String> {
    let file = File::open(path).ok()?;
    let mut archive = zip::ZipArchive::new(BufReader::new(file)).ok()?;
    let name = archive.file_names().find(|n| part(n))?.to_string();
    let mut xml = String::new();
    archive
        .by_name(&name)
        .ok()?
        .take(MAX_XML_PART)
        .read_to_string(&mut xml)
        .ok()?;

    let body = xml_text_runs(&xml, text_tag, para_tag);
    if body.trim().is_empty() {
        return None;
    }
    let mut chars = body.chars();
    let mut text: String = chars.by_ref().take(2000).collect();
    if chars.next().is_some() {
        text.push_str("\n\n[Document truncated for preview]");
    }
    Some(format!("{title}\n{}\n\n{text}", "─".repeat(30)))
}

/// Concatenate the text inside `text_tag` elements, with a newline after each
/// `para_tag` element.
pub(crate) fn xml_text_runs(xml: &str, text_tag: &[u8], para_tag: Option<&[u8]>) -> String {
    let mut reader = XmlReader::from_str(xml);
    let mut out = String::new();
    let mut in_text = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.name().as_ref().as_bytes() == text_tag => in_text = true,
            Ok(Event::End(e)) if e.name().as_ref().as_bytes() == text_tag => in_text = false,
            Ok(Event::End(e)) if Some(e.name().as_ref().as_bytes()) == para_tag => out.push('\n'),
            Ok(Event::Text(e)) if in_text => out.push_str(&e),
            Ok(Event::GeneralRef(r)) if in_text => {
                if let Ok(Some(c)) = r.resolve_char_ref() {
                    out.push(c);
                } else {
                    out.push_str(match r.as_ref() {
                        "amp" => "&",
                        "lt" => "<",
                        "gt" => ">",
                        "quot" => "\"",
                        "apos" => "'",
                        _ => "",
                    });
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zip_bomb_spreadsheet_is_not_opened() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bomb.xlsx");
        let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("xl/worksheets/sheet1.xml", opts).unwrap();
        let chunk = vec![b' '; 1024 * 1024];
        for _ in 0..(MAX_SPREADSHEET_UNPACKED / chunk.len() as u64 + 1) {
            zip.write_all(&chunk).unwrap();
        }
        zip.finish().unwrap();
        assert!(fs::metadata(&path).unwrap().len() < MAX_SPREADSHEET_FILE);
        assert_eq!(
            load_preview_text(&path),
            "[Spreadsheet too large to preview]"
        );
    }

    #[test]
    fn xml_runs_keep_spaces_and_entities() {
        let xml = r#"<w:body><w:p><w:r><w:t xml:space="preserve">Fish &amp; </w:t></w:r><w:r><w:t>chips&#33;</w:t></w:r></w:p><w:p><w:r><w:t>Next</w:t></w:r></w:p></w:body>"#;
        assert_eq!(
            xml_text_runs(xml, b"w:t", Some(b"w:p")),
            "Fish & chips!\nNext\n"
        );
    }

    #[test]
    fn text_preview_handles_multibyte_and_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.txt");
        fs::write(&f, "é".repeat(TEXT_PREVIEW_CHARS + 10)).unwrap();
        let p = load_preview_text(&f);
        assert!(p.starts_with("éé"));
        assert!(p.ends_with("[File truncated for preview]"));

        let bin = dir.path().join("b.txt");
        fs::write(&bin, b"ab\0cd").unwrap();
        assert_eq!(load_preview_text(&bin), "[Binary file]");
    }
}
