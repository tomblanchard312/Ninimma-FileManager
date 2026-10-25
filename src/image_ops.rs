// Ninimma modification: independent project branding and integration.
//! Image rotation, in the style of Explorer's "Rotate left / Rotate right".
//!
//! JPEGs are rotated losslessly by rewriting the EXIF Orientation tag, the same
//! approach Windows uses: no pixels are re-encoded, so no quality is lost and
//! it is instant even for large photos. Lossless formats (PNG, BMP, TIFF, ...)
//! are decoded, rotated and re-encoded in the same format.

use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};
use std::fs;
use std::io::{Read, Write};
use std::path::Path;

pub(crate) const MAX_SOURCE_BYTES: u64 = 200_000_000;

pub(crate) fn decode_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(256 * 1024 * 1024);
    limits
}

pub(crate) fn limit_decoder(decoder: &mut impl ImageDecoder) -> image::ImageResult<()> {
    // from_decoder bypasses ImageReader::decode's output-buffer budget check.
    let mut limits = decode_limits();
    limits.reserve(decoder.total_bytes())?;
    decoder.set_limits(limits)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rotation {
    Clockwise,
    CounterClockwise,
    /// Mirror left to right.
    FlipHorizontal,
    /// Mirror top to bottom.
    FlipVertical,
}

impl Rotation {
    /// What undoes it.
    pub fn inverse(self) -> Self {
        match self {
            Rotation::Clockwise => Rotation::CounterClockwise,
            Rotation::CounterClockwise => Rotation::Clockwise,
            flip => flip,
        }
    }
}

/// Formats we can rotate without losing quality.
pub fn can_rotate(path: &Path) -> bool {
    // WebP is left out: most WebP photos are lossy, and rotating them without
    // quality loss isn't possible.
    matches!(
        extension(path).as_deref(),
        Some("jpg" | "jpeg" | "jpe" | "jfif" | "png" | "bmp" | "tif" | "tiff" | "tga" | "qoi")
    )
}

pub fn rotate_file(path: &Path, rotation: Rotation) -> Result<(), String> {
    match extension(path).as_deref() {
        Some("jpg" | "jpeg" | "jpe" | "jfif") => rotate_jpeg_exif(path, rotation),
        _ if can_rotate(path) => rotate_pixels(path, rotation),
        _ => Err("This file type can't be rotated".to_string()),
    }
}

fn extension(path: &Path) -> Option<String> {
    path.extension().map(|e| e.to_string_lossy().to_lowercase())
}

// ---------------------------------------------------------------------------
// Orientation arithmetic
//
// Each EXIF orientation is an element of the dihedral group D4, modelled here
// as a 2x2 integer matrix. Rotating the *displayed* image by R turns a stored
// orientation T into R·T.
// ---------------------------------------------------------------------------

type Mat = [[i8; 2]; 2];

const IDENTITY: Mat = [[1, 0], [0, 1]];
const ROT90: Mat = [[0, -1], [1, 0]];
const FLIP_H: Mat = [[-1, 0], [0, 1]];

fn mul(a: Mat, b: Mat) -> Mat {
    let mut out = [[0; 2]; 2];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = a[i][0] * b[0][j] + a[i][1] * b[1][j];
        }
    }
    out
}

/// Matrix matching `DynamicImage::apply_orientation`'s order of operations.
fn orientation_matrix(o: Orientation) -> Mat {
    let r90 = ROT90;
    let r180 = mul(r90, r90);
    let r270 = mul(r180, r90);
    match o {
        Orientation::NoTransforms => IDENTITY,
        Orientation::Rotate90 => r90,
        Orientation::Rotate180 => r180,
        Orientation::Rotate270 => r270,
        Orientation::FlipHorizontal => FLIP_H,
        Orientation::FlipVertical => mul(FLIP_H, r180),
        Orientation::Rotate90FlipH => mul(FLIP_H, r90),
        Orientation::Rotate270FlipH => mul(FLIP_H, r270),
    }
}

const ALL_ORIENTATIONS: [Orientation; 8] = [
    Orientation::NoTransforms,
    Orientation::Rotate90,
    Orientation::Rotate180,
    Orientation::Rotate270,
    Orientation::FlipHorizontal,
    Orientation::FlipVertical,
    Orientation::Rotate90FlipH,
    Orientation::Rotate270FlipH,
];

/// The orientation that results from rotating an image already displayed with
/// `current` by a further quarter turn.
pub fn compose(current: Orientation, rotation: Rotation) -> Orientation {
    let r = match rotation {
        Rotation::Clockwise => ROT90,
        Rotation::CounterClockwise => mul(mul(ROT90, ROT90), ROT90),
        Rotation::FlipHorizontal => FLIP_H,
        Rotation::FlipVertical => mul(FLIP_H, mul(ROT90, ROT90)),
    };
    let target = mul(r, orientation_matrix(current));
    ALL_ORIENTATIONS
        .into_iter()
        .find(|o| orientation_matrix(*o) == target)
        .expect("D4 is closed under composition")
}

// ---------------------------------------------------------------------------
// JPEG: lossless rotation via the EXIF Orientation tag
// ---------------------------------------------------------------------------

const ORIENTATION_TAG: u16 = 0x0112;
const TYPE_SHORT: u16 = 3;
const EXIF_HEADER: &[u8] = b"Exif\0\0";

#[derive(Clone, Copy)]
enum Endian {
    Little,
    Big,
}

impl Endian {
    fn u16(self, b: &[u8]) -> u16 {
        match self {
            Endian::Little => u16::from_le_bytes([b[0], b[1]]),
            Endian::Big => u16::from_be_bytes([b[0], b[1]]),
        }
    }
    fn u32(self, b: &[u8]) -> u32 {
        match self {
            Endian::Little => u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            Endian::Big => u32::from_be_bytes([b[0], b[1], b[2], b[3]]),
        }
    }
    fn put_u16(self, v: u16) -> [u8; 2] {
        match self {
            Endian::Little => v.to_le_bytes(),
            Endian::Big => v.to_be_bytes(),
        }
    }
    fn put_u32(self, v: u32) -> [u8; 4] {
        match self {
            Endian::Little => v.to_le_bytes(),
            Endian::Big => v.to_be_bytes(),
        }
    }
}

fn rotate_jpeg_exif(path: &Path, rotation: Rotation) -> Result<(), String> {
    let data = fs::read(path).map_err(|e| format!("Couldn't read file: {e}"))?;
    let current = read_jpeg_orientation(&data);
    let new = compose(current, rotation).to_exif() as u16;
    let updated = set_jpeg_orientation(&data, new)?;
    write_atomically(path, &updated)
}

fn read_jpeg_orientation(data: &[u8]) -> Orientation {
    Orientation::from_exif_chunk(
        find_exif(data)
            .map(|(start, len)| &data[start..start + len])
            .unwrap_or(&[]),
    )
    .unwrap_or(Orientation::NoTransforms)
}

/// Iterate JPEG marker segments before the image data.
/// Yields (marker, segment start including the 0xFF marker bytes, payload start, payload length).
fn segments(data: &[u8]) -> impl Iterator<Item = (u8, usize, usize, usize)> + '_ {
    let mut pos = 2; // skip SOI
    std::iter::from_fn(move || {
        if pos + 4 > data.len() || data[pos] != 0xFF {
            return None;
        }
        let marker = data[pos + 1];
        // Start of scan or end of image: no more metadata segments.
        if marker == 0xDA || marker == 0xD9 {
            return None;
        }
        let len = u16::from_be_bytes([data[pos + 2], data[pos + 3]]) as usize;
        if len < 2 || pos + 2 + len > data.len() {
            return None;
        }
        let item = (marker, pos, pos + 4, len - 2);
        pos += 2 + len;
        Some(item)
    })
}

/// Location (start, length) of the TIFF block inside the APP1 Exif segment.
fn find_exif(data: &[u8]) -> Option<(usize, usize)> {
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return None;
    }
    segments(data)
        .find(|&(marker, _, start, len)| {
            marker == 0xE1 && len >= 6 && &data[start..start + 6] == EXIF_HEADER
        })
        .map(|(_, _, start, len)| (start + 6, len - 6))
}

fn set_jpeg_orientation(data: &[u8], orientation: u16) -> Result<Vec<u8>, String> {
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return Err("Not a valid JPEG file".to_string());
    }

    let Some((tiff_start, tiff_len)) = find_exif(data) else {
        return Ok(insert_minimal_exif(data, orientation));
    };
    let tiff = &data[tiff_start..tiff_start + tiff_len];
    let endian = match tiff.get(0..4) {
        Some(b"II*\0") => Endian::Little,
        Some(b"MM\0*") => Endian::Big,
        _ => return Err("Unrecognised EXIF data".to_string()),
    };
    let corrupt = || "Corrupt EXIF data".to_string();
    let ifd0 = endian.u32(tiff.get(4..8).ok_or_else(corrupt)?) as usize;
    let count = tiff
        .get(ifd0..ifd0.checked_add(2).ok_or_else(corrupt)?)
        .map(|b| endian.u16(b) as usize)
        .ok_or_else(corrupt)?;
    let entries_end = ifd0 + 2 + count * 12;
    if entries_end + 4 > tiff.len() {
        return Err(corrupt());
    }

    // Common case: the tag exists, so overwrite its two bytes in place.
    for i in 0..count {
        let entry = ifd0 + 2 + i * 12;
        if endian.u16(&tiff[entry..]) == ORIENTATION_TAG {
            let mut out = data.to_vec();
            let value_at = tiff_start + entry + 8;
            out[value_at..value_at + 2].copy_from_slice(&endian.put_u16(orientation));
            // A SHORT that fits inline is left-justified; zero the rest.
            out[value_at + 2..value_at + 4].copy_from_slice(&[0, 0]);
            let type_at = tiff_start + entry + 2;
            out[type_at..type_at + 2].copy_from_slice(&endian.put_u16(TYPE_SHORT));
            out[type_at + 2..type_at + 6].copy_from_slice(&endian.put_u32(1));
            return Ok(out);
        }
    }

    // EXIF exists but has no Orientation tag. Write a copy of IFD0 with the tag
    // added at the end of the TIFF block and repoint the header at it. Every
    // other offset stays valid because nothing before it moves.
    let mut new_tiff = tiff.to_vec();
    if new_tiff.len() % 2 == 1 {
        new_tiff.push(0); // IFDs must start on a word boundary
    }
    let new_ifd = new_tiff.len();
    let mut entries: Vec<[u8; 12]> = (0..count)
        .map(|i| {
            tiff[ifd0 + 2 + i * 12..ifd0 + 14 + i * 12]
                .try_into()
                .unwrap()
        })
        .collect();
    let mut tag_entry = [0u8; 12];
    tag_entry[0..2].copy_from_slice(&endian.put_u16(ORIENTATION_TAG));
    tag_entry[2..4].copy_from_slice(&endian.put_u16(TYPE_SHORT));
    tag_entry[4..8].copy_from_slice(&endian.put_u32(1));
    tag_entry[8..10].copy_from_slice(&endian.put_u16(orientation));
    // Keep entries sorted by tag, as the TIFF spec requires.
    let insert_at = entries
        .iter()
        .position(|e| endian.u16(e) > ORIENTATION_TAG)
        .unwrap_or(entries.len());
    entries.insert(insert_at, tag_entry);

    new_tiff.extend_from_slice(&endian.put_u16(entries.len() as u16));
    for e in &entries {
        new_tiff.extend_from_slice(e);
    }
    new_tiff.extend_from_slice(&tiff[entries_end..entries_end + 4]); // next-IFD pointer
    new_tiff[4..8].copy_from_slice(&endian.put_u32(new_ifd as u32));

    let segment_len = 2 + EXIF_HEADER.len() + new_tiff.len();
    if segment_len > u16::MAX as usize {
        return Err("EXIF block too large to update".to_string());
    }
    let app1_start = tiff_start - EXIF_HEADER.len() - 4;
    let app1_end = tiff_start + tiff_len;
    let mut out = Vec::with_capacity(data.len() + new_tiff.len() - tiff_len);
    out.extend_from_slice(&data[..app1_start]);
    out.extend_from_slice(&[0xFF, 0xE1]);
    out.extend_from_slice(&(segment_len as u16).to_be_bytes());
    out.extend_from_slice(EXIF_HEADER);
    out.extend_from_slice(&new_tiff);
    out.extend_from_slice(&data[app1_end..]);
    Ok(out)
}

/// For JPEGs with no EXIF at all, add an APP1 segment holding only the tag.
fn insert_minimal_exif(data: &[u8], orientation: u16) -> Vec<u8> {
    let e = Endian::Big;
    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"MM\0*");
    tiff.extend_from_slice(&e.put_u32(8));
    tiff.extend_from_slice(&e.put_u16(1));
    tiff.extend_from_slice(&e.put_u16(ORIENTATION_TAG));
    tiff.extend_from_slice(&e.put_u16(TYPE_SHORT));
    tiff.extend_from_slice(&e.put_u32(1));
    tiff.extend_from_slice(&e.put_u16(orientation));
    tiff.extend_from_slice(&[0, 0]);
    tiff.extend_from_slice(&e.put_u32(0));

    // JFIF requires APP0 to come first, so insert after it if present.
    let insert_at = segments(data)
        .next()
        .filter(|&(marker, ..)| marker == 0xE0)
        .map(|(_, _, start, len)| start + len)
        .unwrap_or(2);

    let segment_len = (2 + EXIF_HEADER.len() + tiff.len()) as u16;
    let mut out = Vec::with_capacity(data.len() + segment_len as usize + 2);
    out.extend_from_slice(&data[..insert_at]);
    out.extend_from_slice(&[0xFF, 0xE1]);
    out.extend_from_slice(&segment_len.to_be_bytes());
    out.extend_from_slice(EXIF_HEADER);
    out.extend_from_slice(&tiff);
    out.extend_from_slice(&data[insert_at..]);
    out
}

// ---------------------------------------------------------------------------
// Other formats: rotate the pixels and re-encode losslessly
// ---------------------------------------------------------------------------

fn rotate_pixels(path: &Path, rotation: Rotation) -> Result<(), String> {
    let mut original = Vec::new();
    fs::File::open(path)
        .and_then(|f| f.take(MAX_SOURCE_BYTES + 1).read_to_end(&mut original))
        .map_err(|e| format!("Couldn't read file: {e}"))?;
    if original.len() as u64 > MAX_SOURCE_BYTES {
        return Err("Image source exceeds the 200 MB rotation limit".into());
    }
    refuse_multi_frame(&original)?;
    let mut reader = ImageReader::new(std::io::Cursor::new(&original))
        .with_guessed_format()
        .map_err(|e| format!("Couldn't open image: {e}"))?;
    reader.limits(decode_limits());
    let format = reader.format().ok_or("Unknown image format")?;
    let mut decoder = reader
        .into_decoder()
        .map_err(|e| format!("Couldn't decode image: {e}"))?;
    limit_decoder(&mut decoder).map_err(|e| format!("Couldn't decode image: {e}"))?;
    let existing = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let icc = decoder.icc_profile().ok().flatten();
    let mut img =
        DynamicImage::from_decoder(decoder).map_err(|e| format!("Couldn't decode image: {e}"))?;
    // Bake in any orientation the file already declared, since re-encoding drops it.
    img.apply_orientation(existing);
    let rotated = match rotation {
        Rotation::Clockwise => img.rotate90(),
        Rotation::CounterClockwise => img.rotate270(),
        Rotation::FlipHorizontal => img.fliph(),
        Rotation::FlipVertical => img.flipv(),
    };
    let encoded = encode(&rotated, format, icc).map_err(|e| format!("Couldn't save image: {e}"))?;
    write_atomically(path, &encoded)
}

/// Re-encoding would keep only the first frame of an animated PNG or the
/// first page of a multi-page TIFF, so refuse rather than lose data.
fn refuse_multi_frame(data: &[u8]) -> Result<(), String> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        let mut pos = 8;
        while pos + 8 <= data.len() {
            let len = u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]])
                as usize;
            match &data[pos + 4..pos + 8] {
                b"acTL" => return Err("Animated PNGs can't be rotated".to_string()),
                b"IDAT" => break,
                _ => pos = pos.saturating_add(12).saturating_add(len),
            }
        }
    }
    let endian = match data.get(0..4) {
        Some(b"II*\0") => Some(Endian::Little),
        Some(b"MM\0*") => Some(Endian::Big),
        _ => None,
    };
    if let Some(e) = endian {
        let next = (|| {
            let ifd0 = e.u32(data.get(4..8)?) as usize;
            let count = e.u16(data.get(ifd0..ifd0 + 2)?) as usize;
            let at = ifd0 + 2 + count * 12;
            Some(e.u32(data.get(at..at + 4)?))
        })();
        if next.is_some_and(|n| n != 0) {
            return Err("Multi-page TIFFs can't be rotated".to_string());
        }
    }
    Ok(())
}

fn encode(
    img: &DynamicImage,
    format: ImageFormat,
    icc: Option<Vec<u8>>,
) -> image::ImageResult<Vec<u8>> {
    use image::ImageEncoder;
    let mut out = std::io::Cursor::new(Vec::new());
    match format {
        ImageFormat::Png => {
            let mut enc = image::codecs::png::PngEncoder::new(&mut out);
            if let Some(icc) = icc {
                let _ = enc.set_icc_profile(icc);
            }
            img.write_with_encoder(enc)?;
        }
        ImageFormat::Tiff => {
            let mut enc = image::codecs::tiff::TiffEncoder::new(&mut out);
            if let Some(icc) = icc {
                let _ = enc.set_icc_profile(icc);
            }
            img.write_with_encoder(enc)?;
        }
        _ => img.write_to(&mut out, format)?,
    }
    Ok(out.into_inner())
}

/// Write via a fresh temporary file and rename, so a crash or full disk can
/// never leave a half-written photo behind. The temp file has an
/// unpredictable name and is created exclusively, so a pre-planted symlink
/// can't redirect the write elsewhere.
fn write_atomically(path: &Path, data: &[u8]) -> Result<(), String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut attempt = 0u32;
    let (tmp, mut file) = loop {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let tmp = path.with_file_name(format!(
            ".{name}.{}.{nanos:x}{attempt}.tmp",
            std::process::id()
        ));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
        {
            Ok(f) => break (tmp, f),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && attempt < 16 => attempt += 1,
            Err(e) => return Err(format!("Couldn't write file: {e}")),
        }
    };
    let result = file
        .write_all(data)
        .and_then(|_| file.sync_all())
        .map_err(|e| format!("Couldn't write file: {e}"))
        .and_then(|_| {
            drop(file);
            if let Ok(meta) = fs::metadata(path) {
                let _ = fs::set_permissions(&tmp, meta.permissions());
            }
            // Replaces the original on every platform (MoveFileExW with
            // MOVEFILE_REPLACE_EXISTING on Windows).
            fs::rename(&tmp, path).map_err(|e| format!("Couldn't replace file: {e}"))
        });
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_decoded_image_is_refused_without_replacing_the_source() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("large.bmp");
        DynamicImage::new_rgb8(1, 1).save(&file).unwrap();
        let mut bytes = fs::read(&file).unwrap();
        bytes[18..22].copy_from_slice(&20_000i32.to_le_bytes());
        bytes[22..26].copy_from_slice(&20_000i32.to_le_bytes());
        fs::write(&file, &bytes).unwrap();
        let error = rotate_file(&file, Rotation::Clockwise).unwrap_err();
        assert!(error.to_lowercase().contains("limit"), "{error}");
        assert!(crate::thumbnail::decode_oriented(&file).is_none());
        assert_eq!(fs::read(&file).unwrap(), bytes);
    }

    use image::{GenericImageView, Rgb, RgbImage};

    /// A 3x2 image with every pixel distinct, so any wrong transform is visible.
    fn sample() -> DynamicImage {
        let mut img = RgbImage::new(3, 2);
        for (x, y, p) in img.enumerate_pixels_mut() {
            *p = Rgb([(x * 80) as u8, (y * 120) as u8, 7]);
        }
        DynamicImage::ImageRgb8(img)
    }

    fn displayed(o: Orientation) -> DynamicImage {
        let mut img = sample();
        img.apply_orientation(o);
        img
    }

    #[test]
    fn compose_matches_actual_pixel_rotation() {
        for o in ALL_ORIENTATIONS {
            assert_eq!(
                displayed(compose(o, Rotation::Clockwise)).to_rgb8(),
                displayed(o).rotate90().to_rgb8(),
                "clockwise from {o:?}"
            );
            assert_eq!(
                displayed(compose(o, Rotation::CounterClockwise)).to_rgb8(),
                displayed(o).rotate270().to_rgb8(),
                "counter-clockwise from {o:?}"
            );
        }
    }

    #[test]
    fn four_turns_is_identity() {
        let mut o = Orientation::NoTransforms;
        for _ in 0..4 {
            o = compose(o, Rotation::Clockwise);
        }
        assert_eq!(o, Orientation::NoTransforms);
    }

    fn jpeg_bytes() -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        sample().write_to(&mut out, ImageFormat::Jpeg).unwrap();
        out.into_inner()
    }

    fn decoded_orientation(bytes: &[u8]) -> Orientation {
        let mut dec = ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .unwrap()
            .into_decoder()
            .unwrap();
        dec.orientation().unwrap()
    }

    #[test]
    fn jpeg_without_exif_gains_orientation_tag() {
        let original = jpeg_bytes();
        assert!(find_exif(&original).is_none());
        let rotated = set_jpeg_orientation(&original, 6).unwrap();
        assert_eq!(decoded_orientation(&rotated), Orientation::Rotate90);
        // Pixel data is untouched: the compressed scan is byte-identical.
        assert!(rotated.ends_with(&original[original.len() - 64..]));
    }

    #[test]
    fn jpeg_existing_tag_is_rewritten_in_place() {
        let once = set_jpeg_orientation(&jpeg_bytes(), 6).unwrap();
        let twice = set_jpeg_orientation(&once, 3).unwrap();
        assert_eq!(once.len(), twice.len());
        assert_eq!(decoded_orientation(&twice), Orientation::Rotate180);
    }

    #[test]
    fn jpeg_exif_without_orientation_tag_gets_one_added() {
        // Build EXIF holding only an unrelated tag (0x010F Make, inline "ab\0").
        let e = Endian::Little;
        let mut tiff = b"II*\0".to_vec();
        tiff.extend_from_slice(&e.put_u32(8));
        tiff.extend_from_slice(&e.put_u16(1));
        tiff.extend_from_slice(&e.put_u16(0x010F));
        tiff.extend_from_slice(&e.put_u16(2));
        tiff.extend_from_slice(&e.put_u32(3));
        tiff.extend_from_slice(b"ab\0\0");
        tiff.extend_from_slice(&e.put_u32(0));
        let jpeg = jpeg_bytes();
        let mut data = jpeg[..2].to_vec();
        data.extend_from_slice(&[0xFF, 0xE1]);
        data.extend_from_slice(&((2 + 6 + tiff.len()) as u16).to_be_bytes());
        data.extend_from_slice(EXIF_HEADER);
        data.extend_from_slice(&tiff);
        data.extend_from_slice(&jpeg[2..]);
        assert_eq!(decoded_orientation(&data), Orientation::NoTransforms);

        let updated = set_jpeg_orientation(&data, 8).unwrap();
        assert_eq!(decoded_orientation(&updated), Orientation::Rotate270);
        image::load_from_memory(&updated).expect("still a valid JPEG");
    }

    #[test]
    fn flips_mirror_pixels_and_undo_themselves() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("a.png");
        sample().save(&png).unwrap();
        rotate_file(&png, Rotation::FlipHorizontal).unwrap();
        assert_eq!(
            image::open(&png).unwrap().to_rgb8(),
            sample().fliph().to_rgb8()
        );
        rotate_file(&png, Rotation::FlipHorizontal.inverse()).unwrap();
        assert_eq!(image::open(&png).unwrap().to_rgb8(), sample().to_rgb8());

        // JPEGs flip through the EXIF tag: shown mirrored, no re-encoding.
        let jpg = dir.path().join("b.jpg");
        fs::write(&jpg, jpeg_bytes()).unwrap();
        rotate_file(&jpg, Rotation::FlipVertical).unwrap();
        assert_eq!(
            decoded_orientation(&fs::read(&jpg).unwrap()),
            Orientation::FlipVertical
        );
        rotate_file(&jpg, Rotation::FlipVertical).unwrap();
        assert_eq!(
            decoded_orientation(&fs::read(&jpg).unwrap()),
            Orientation::NoTransforms
        );
    }

    #[test]
    fn rotate_file_round_trips_png_and_jpeg() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("a.png");
        sample().save(&png).unwrap();
        rotate_file(&png, Rotation::Clockwise).unwrap();
        assert_eq!(image::open(&png).unwrap().dimensions(), (2, 3));
        assert_eq!(
            image::open(&png).unwrap().to_rgb8(),
            sample().rotate90().to_rgb8()
        );

        let jpg = dir.path().join("b.JPG");
        fs::write(&jpg, jpeg_bytes()).unwrap();
        rotate_file(&jpg, Rotation::CounterClockwise).unwrap();
        assert_eq!(
            decoded_orientation(&fs::read(&jpg).unwrap()),
            Orientation::Rotate270
        );
        let leftovers = fs::read_dir(dir.path())
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            })
            .count();
        assert_eq!(leftovers, 0, "no temp files left behind");
    }

    #[test]
    fn truncated_exif_is_an_error_not_a_panic() {
        let jpeg = jpeg_bytes();
        for tiff in [
            &b"II*\0"[..],
            &b"MM\0*\0\0"[..],
            &b"II*\0\xff\xff\xff\x7f"[..],
        ] {
            let mut data = jpeg[..2].to_vec();
            data.extend_from_slice(&[0xFF, 0xE1]);
            data.extend_from_slice(&((2 + 6 + tiff.len()) as u16).to_be_bytes());
            data.extend_from_slice(EXIF_HEADER);
            data.extend_from_slice(tiff);
            data.extend_from_slice(&jpeg[2..]);
            assert!(set_jpeg_orientation(&data, 6).is_err());
        }
    }

    #[test]
    fn animated_png_and_multipage_tiff_are_refused() {
        let mut apng = b"\x89PNG\r\n\x1a\n".to_vec();
        apng.extend_from_slice(&[0, 0, 0, 13]);
        apng.extend_from_slice(b"IHDR");
        apng.extend_from_slice(&[0; 13 + 4]);
        apng.extend_from_slice(&[0, 0, 0, 8]);
        apng.extend_from_slice(b"acTL");
        assert!(refuse_multi_frame(&apng).is_err());

        let mut tiff = b"II*\0".to_vec();
        tiff.extend_from_slice(&8u32.to_le_bytes());
        tiff.extend_from_slice(&0u16.to_le_bytes()); // no entries
        tiff.extend_from_slice(&100u32.to_le_bytes()); // a second page follows
        assert!(refuse_multi_frame(&tiff).is_err());
        tiff.truncate(10);
        tiff.extend_from_slice(&0u32.to_le_bytes());
        assert!(refuse_multi_frame(&tiff).is_ok());
    }

    #[test]
    fn png_icc_profile_survives_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("icc.png");
        let mut out = std::io::Cursor::new(Vec::new());
        encode(&sample(), ImageFormat::Png, Some(vec![1, 2, 3, 4]))
            .map(|b| out.get_mut().extend(b))
            .unwrap();
        fs::write(&png, out.into_inner()).unwrap();
        rotate_file(&png, Rotation::Clockwise).unwrap();
        let mut dec = ImageReader::open(&png)
            .unwrap()
            .with_guessed_format()
            .unwrap()
            .into_decoder()
            .unwrap();
        assert_eq!(dec.icc_profile().unwrap(), Some(vec![1, 2, 3, 4]));
    }

    #[test]
    fn unsupported_formats_are_rejected() {
        assert!(!can_rotate(Path::new("x.webp")));
        assert!(!can_rotate(Path::new("x.gif")));
        assert!(!can_rotate(Path::new("x.heic")));
        assert!(rotate_file(Path::new("x.gif"), Rotation::Clockwise).is_err());
    }
}
