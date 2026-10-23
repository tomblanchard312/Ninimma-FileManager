// Ninimma modification: independent project branding and integration.
//! Realistic test files made from scratch: text PDFs, scanned (image-only)
//! PDFs, Word documents, phone photos of receipts and ordinary photos, with
//! EXIF dates and GPS positions. Nothing is copied from anywhere.

// The evaluation (smart search only) uses most of these.
#![cfg_attr(not(feature = "smart-search"), allow(dead_code))]

use image::{DynamicImage, Rgb, RgbImage};
use std::io::Write;
use std::path::Path;

/// Wraps text to `width` characters per line, keeping given line breaks.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for para in text.lines() {
        let mut line = String::new();
        for word in para.split_whitespace() {
            if !line.is_empty() && line.len() + 1 + word.len() > width {
                lines.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        lines.push(line);
    }
    lines
}

/// Assembles a PDF from its objects (1-based, object 1 the catalog).
fn pdf_from(objects: Vec<Vec<u8>>) -> Vec<u8> {
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n", i + 1).bytes());
        out.extend(o);
        out.extend(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).bytes());
    for o in offsets {
        out.extend(format!("{o:010} 00000 n \n").bytes());
    }
    out.extend(
        format!(
            "trailer<</Size {}/Root 1 0 R>>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .bytes(),
    );
    out
}

/// A one-page PDF with real (selectable) text. `narrow` makes a receipt-
/// shaped page in a typewriter font; otherwise a letter-size page.
pub fn text_pdf(text: &str, narrow: bool) -> Vec<u8> {
    let (w, h, font, size, chars) = if narrow {
        (300, 560, "Courier-Bold", 13, 34)
    } else {
        (612, 792, "Helvetica", 11, 90)
    };
    let mut stream = format!("BT /F1 {size} Tf 36 {} Td {} TL\n", h - 48, size + 5);
    for line in wrap(text, chars) {
        let escaped = line
            .replace('\\', "\\\\")
            .replace('(', "\\(")
            .replace(')', "\\)");
        stream.push_str(&format!("({escaped}) Tj T*\n"));
    }
    stream.push_str("ET");
    pdf_from(vec![
        b"<</Type/Catalog/Pages 2 0 R>>".to_vec(),
        b"<</Type/Pages/Kids[3 0 R]/Count 1>>".to_vec(),
        format!("<</Type/Page/Parent 2 0 R/MediaBox[0 0 {w} {h}]/Contents 4 0 R/Resources<</Font<</F1 5 0 R>>>>>>").into_bytes(),
        format!("<</Length {}>>stream\n{stream}\nendstream", stream.len()).into_bytes(),
        format!("<</Type/Font/Subtype/Type1/BaseFont/{font}>>").into_bytes(),
    ])
}

/// A scanned page: the receipt rendered to pixels, a little grey and
/// uneven like a scan, stored as a JPEG inside a PDF with no text.
pub fn scanned_pdf(text: &str) -> Vec<u8> {
    let image = scan_look(paper(text));
    let jpeg = jpeg_bytes(&image, 85);
    let (w, h) = (image.width(), image.height());
    let content = format!("q 300 0 0 {} 0 0 cm /Im1 Do Q", 300 * h / w);
    let mut image_object = format!("<</Type/XObject/Subtype/Image/Width {w}/Height {h}/ColorSpace/DeviceRGB/BitsPerComponent 8/Filter/DCTDecode/Length {}>>stream\n", jpeg.len()).into_bytes();
    image_object.extend(&jpeg);
    image_object.extend(b"\nendstream");
    pdf_from(vec![
        b"<</Type/Catalog/Pages 2 0 R>>".to_vec(),
        b"<</Type/Pages/Kids[3 0 R]/Count 1>>".to_vec(),
        format!("<</Type/Page/Parent 2 0 R/MediaBox[0 0 300 {}]/Contents 4 0 R/Resources<</XObject<</Im1 5 0 R>>>>>>", 300 * h / w).into_bytes(),
        format!("<</Length {}>>stream\n{content}\nendstream", content.len()).into_bytes(),
        image_object,
    ])
}

/// A Word document, one paragraph per line.
pub fn docx(path: &Path, text: &str) {
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    zip.start_file("[Content_Types].xml", options).unwrap();
    zip.write_all(br#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#).unwrap();
    zip.start_file("word/document.xml", options).unwrap();
    let escape = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let body: String = text
        .lines()
        .map(|l| {
            format!(
                "<w:p><w:r><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>",
                escape(l)
            )
        })
        .collect();
    zip.write_all(format!(r#"<?xml version="1.0" encoding="UTF-8"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}</w:body></w:document>"#).as_bytes()).unwrap();
    zip.finish().unwrap();
}

/// The text printed on receipt-sized paper (via Poppler's pdftoppm).
pub fn paper(text: &str) -> RgbImage {
    let dir = tempfile::tempdir().unwrap();
    let pdf = dir.path().join("page.pdf");
    std::fs::write(&pdf, text_pdf(text, true)).unwrap();
    crate::ocr::render_pdf(
        &pdf,
        &dir.path().join("work"),
        std::time::Duration::from_secs(30),
    )
    .expect("pdftoppm renders the page")
    .remove(0)
    .to_rgb8()
}

/// Greyer paper with uneven light, like a scan or a phone photo.
fn scan_look(image: RgbImage) -> RgbImage {
    let (w, h) = image.dimensions();
    RgbImage::from_fn(w, h, |x, y| {
        let p = image.get_pixel(x, y);
        let shade = 0.88 + 0.08 * ((x as f32 / w as f32) - (y as f32 / h as f32)).abs();
        let warm = |c: u8, k: f32| ((c as f32) * shade * k).min(255.0) as u8;
        Rgb([warm(p[0], 1.0), warm(p[1], 0.98), warm(p[2], 0.93)])
    })
}

/// A phone photo of a receipt: the paper on a dark table.
pub fn receipt_photo(text: &str) -> RgbImage {
    let page = scan_look(paper(text));
    let (pw, ph) = page.dimensions();
    let (w, h) = (pw + pw / 4, ph + ph / 8);
    let mut photo = RgbImage::from_fn(w, h, |x, y| {
        let grain = ((x * 7 + y * 13) % 11) as u8;
        Rgb([52 + grain, 40 + grain, 32 + grain])
    });
    image::imageops::overlay(&mut photo, &page, (pw / 8) as i64, (ph / 16) as i64);
    photo
}

/// An ordinary photo: red rocks under a blue sky (not paper).
pub fn landscape() -> RgbImage {
    RgbImage::from_fn(640, 480, |x, y| {
        let ridge = 260.0 + 60.0 * ((x as f32) / 70.0).sin() + 25.0 * ((x as f32) / 23.0).cos();
        if (y as f32) < ridge {
            let t = y as f32 / ridge;
            Rgb([(90.0 + 60.0 * t) as u8, (150.0 + 50.0 * t) as u8, 235])
        } else {
            let n = ((x * 31 + y * 17) % 23) as u8;
            Rgb([170 + n, 70 + n / 2, 40])
        }
    })
}

fn jpeg_bytes(image: &RgbImage, quality: u8) -> Vec<u8> {
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
        .encode_image(&DynamicImage::ImageRgb8(image.clone()))
        .unwrap();
    out
}

/// Degrees as EXIF's degrees, minutes, seconds.
fn dms(value: f64) -> Vec<exif::Rational> {
    let v = value.abs();
    let d = v.trunc();
    let m = ((v - d) * 60.0).trunc();
    let s = ((v - d) * 60.0 - m) * 60.0;
    vec![
        (d as u32, 1).into(),
        (m as u32, 1).into(),
        ((s * 100.0).round() as u32, 100).into(),
    ]
}

/// A JPEG with the date taken (`YYYY:MM:DD HH:MM:SS`) and, if given, where.
pub fn jpeg_with_exif(image: &RgbImage, taken: &str, place: Option<(f64, f64)>) -> Vec<u8> {
    use exif::experimental::Writer;
    use exif::{Field, In, Tag, Value};
    let field = |tag, value| Field {
        tag,
        ifd_num: In::PRIMARY,
        value,
    };
    let mut fields = vec![field(
        Tag::DateTimeOriginal,
        Value::Ascii(vec![taken.as_bytes().to_vec()]),
    )];
    if let Some((lat, lon)) = place {
        fields.push(field(
            Tag::GPSLatitudeRef,
            Value::Ascii(vec![if lat >= 0.0 {
                b"N".to_vec()
            } else {
                b"S".to_vec()
            }]),
        ));
        fields.push(field(Tag::GPSLatitude, Value::Rational(dms(lat))));
        fields.push(field(
            Tag::GPSLongitudeRef,
            Value::Ascii(vec![if lon >= 0.0 {
                b"E".to_vec()
            } else {
                b"W".to_vec()
            }]),
        ));
        fields.push(field(Tag::GPSLongitude, Value::Rational(dms(lon))));
    }
    let mut writer = Writer::new();
    for f in &fields {
        writer.push_field(f);
    }
    let mut tiff = std::io::Cursor::new(Vec::new());
    writer.write(&mut tiff, false).unwrap();
    let jpeg = jpeg_bytes(image, 90);
    let mut app1 = b"Exif\0\0".to_vec();
    app1.extend_from_slice(&tiff.into_inner());
    let mut out = jpeg[..2].to_vec();
    out.extend_from_slice(&[0xFF, 0xE1]);
    out.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(&app1);
    out.extend_from_slice(&jpeg[2..]);
    out
}
