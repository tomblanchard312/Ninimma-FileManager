// Ninimma modification: independent project branding and integration.
//! System font fallback for scripts egui's bundled fonts don't cover
//! (Chinese, Japanese, Korean, Arabic, Hebrew, Thai, ...).
//!
//! Fonts are only loaded once a file name actually needs them, so users who
//! never see such names pay no memory or startup cost.

use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
use egui::{FontData, FontFamily, FontId};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

/// A font file read from disk: (path, face index in a collection, bytes).
type LoadedFont = (PathBuf, u32, Vec<u8>);

#[derive(Default)]
pub struct FallbackFonts {
    /// Font files already loaded or being loaded.
    requested: HashSet<PathBuf>,
    /// Characters we already looked up (found or not), to avoid repeat work.
    checked: HashSet<char>,
    ready: Arc<Mutex<Vec<LoadedFont>>>,
    /// Listing last scanned; rescanned only when it (or the fonts) change.
    scanned: Option<u64>,
}

impl FallbackFonts {
    /// Call with the names about to be displayed; kicks off loading of a
    /// fallback font for any character the current fonts can't draw.
    pub fn ensure_coverage<'a>(
        &mut self,
        ctx: &egui::Context,
        listing: u64,
        names: impl Iterator<Item = &'a str>,
    ) {
        self.install_ready(ctx);
        if self.scanned == Some(listing) {
            return;
        }
        self.scanned = Some(listing);
        let font = FontId::proportional(14.0);
        let mut missing = Vec::new();
        ctx.fonts_mut(|fonts| {
            for name in names {
                if name.is_ascii() {
                    continue;
                }
                for c in name.chars() {
                    if !c.is_ascii()
                        && !c.is_control()
                        && self.checked.insert(c)
                        && !fonts.has_glyph(&font, c)
                    {
                        missing.push(c);
                    }
                }
            }
        });
        if missing.is_empty() {
            return;
        }

        let mut to_load = Vec::new();
        for c in missing {
            if let Some((path, index)) = find_font_for(c) {
                if self.requested.insert(path.clone()) {
                    to_load.push((path, index));
                }
            }
        }
        if to_load.is_empty() {
            return;
        }
        let ready = self.ready.clone();
        let ctx = ctx.clone();
        thread::spawn(move || {
            for (path, index) in to_load {
                if let Ok(bytes) = std::fs::read(&path) {
                    ready.lock().unwrap().push((path, index, bytes));
                    ctx.request_repaint();
                }
            }
        });
    }

    fn install_ready(&mut self, ctx: &egui::Context) {
        for (path, index, bytes) in self.ready.lock().unwrap().drain(..) {
            let mut data = FontData::from_owned(bytes);
            data.index = index;
            ctx.add_font(FontInsert {
                name: format!("fallback:{}#{index}", path.display()),
                data,
                families: [FontFamily::Proportional, FontFamily::Monospace]
                    .into_iter()
                    .map(|family| InsertFontFamily {
                        family,
                        priority: FontPriority::Lowest,
                    })
                    .collect(),
            });
            // Now that more glyphs exist, recheck everything next time.
            self.checked.clear();
            self.scanned = None;
        }
    }
}

/// The interface's icons: Lucide (ISC), at the code points of the emoji
/// the UI's strings use (built by packaging/icons/build-icon-font.py).
const ICONS: &[u8] = include_bytes!("../assets/fonts/icons.ttf");

/// Puts the icon font first in the proportional family, so its icons win
/// even where the text font has the same character (arrows, stars); its
/// vertical metrics are the text font's (see the build script), so line
/// heights don't change. Monospace text (Hack, other metrics) gets it
/// after its own font. The emoji fonts stay as the fallback. Lucide draws
/// a whole em above the baseline, so it's scaled down a little and lowered
/// to sit on the text's centre line.
pub fn install_icons(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let data = FontData::from_static(ICONS).tweak(egui::FontTweak {
        scale: 0.95,
        y_offset_factor: 0.12,
        ..Default::default()
    });
    fonts
        .font_data
        .insert("icons".to_owned(), std::sync::Arc::new(data));
    if let Some(list) = fonts.families.get_mut(&FontFamily::Proportional) {
        list.insert(0, "icons".to_owned());
    }
    if let Some(list) = fonts.families.get_mut(&FontFamily::Monospace) {
        let at = list.len().min(1);
        list.insert(at, "icons".to_owned());
    }
    ctx.set_fonts(fonts);
}

/// Locate an installed font that contains `c`.
#[cfg(all(unix, not(target_os = "macos")))]
fn find_font_for(c: char) -> Option<(PathBuf, u32)> {
    let pattern = format!("sans-serif:charset={:x}", c as u32);
    let out = std::process::Command::new("fc-match")
        .args(["-s", "-f", "%{file}|%{index}\n", &pattern])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .take(3)
        .find_map(|line| {
            let (file, index) = line.split_once('|')?;
            // egui can't read WOFF/Type1; take the first TrueType/OpenType match.
            let lower = file.to_lowercase();
            if !(lower.ends_with(".ttf") || lower.ends_with(".otf") || lower.ends_with(".ttc")) {
                return None;
            }
            Some((PathBuf::from(file), index.parse().unwrap_or(0)))
        })
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
fn find_font_for(c: char) -> Option<(PathBuf, u32)> {
    #[cfg(windows)]
    let candidates: &[(&str, u32)] = {
        let cjk = matches!(c as u32, 0x2E80..=0x9FFF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF);
        let hangul = matches!(c as u32, 0x1100..=0x11FF | 0xAC00..=0xD7AF);
        if hangul {
            &[(r"C:\Windows\Fonts\malgun.ttf", 0)]
        } else if cjk {
            &[
                (r"C:\Windows\Fonts\msyh.ttc", 0),
                (r"C:\Windows\Fonts\YuGothM.ttc", 0),
                (r"C:\Windows\Fonts\simsun.ttc", 0),
            ]
        } else {
            // Segoe UI covers Arabic, Hebrew, Thai and most other scripts.
            &[
                (r"C:\Windows\Fonts\segoeui.ttf", 0),
                (r"C:\Windows\Fonts\arial.ttf", 0),
            ]
        }
    };
    #[cfg(target_os = "macos")]
    let candidates: &[(&str, u32)] = {
        let _ = c;
        &[
            ("/System/Library/Fonts/Supplemental/Arial Unicode.ttf", 0),
            ("/System/Library/Fonts/Hiragino Sans GB.ttc", 0),
            ("/System/Library/Fonts/AppleSDGothicNeo.ttc", 0),
        ]
    };
    candidates
        .iter()
        .map(|(p, i)| (PathBuf::from(p), *i))
        .find(|(p, _)| p.is_file())
}

#[cfg(test)]
mod icon_tests {
    use super::*;

    /// The glyphs packaging/icons/map.txt gives icons.
    fn mapped() -> Vec<char> {
        include_str!("../packaging/icons/map.txt")
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .filter_map(|l| l.chars().next())
            .collect()
    }

    #[test]
    fn every_mapped_glyph_is_in_the_icon_font() {
        let mut fonts = egui::FontDefinitions::empty();
        fonts.font_data.insert(
            "icons".to_owned(),
            std::sync::Arc::new(FontData::from_static(ICONS)),
        );
        let only = FontFamily::Name("icons-only".into());
        fonts
            .families
            .insert(only.clone(), vec!["icons".to_owned()]);
        let ctx = egui::Context::default();
        ctx.set_fonts(fonts);
        ctx.run_ui(Default::default(), |_| {})
            .textures_delta
            .clear();
        let missing: Vec<char> = mapped()
            .into_iter()
            .filter(|c| !ctx.fonts_mut(|f| f.has_glyph(&FontId::new(14.0, only.clone()), *c)))
            .collect();
        assert!(
            missing.is_empty(),
            "rebuild assets/fonts/icons.ttf: {missing:?}"
        );
    }

    #[test]
    fn every_icon_the_interface_uses_has_a_lucide_icon() {
        // Pictographs and symbols (not letters, punctuation or box drawing).
        let icon_like = |c: char| {
            matches!(c as u32,
                0x1F300..=0x1FAFF | 0x2190..=0x21FF | 0x2200..=0x22FF | 0x2300..=0x23FF | 0x25A0..=0x27BF
                | 0x27F0..=0x27FF | 0x2B00..=0x2BFF)
        };
        // Drawn as text on purpose.
        let text = ['≈', '…', '✔', '▲', '▼'];
        let mapped = mapped();
        let mut missing = Vec::new();
        for source in [
            include_str!("ui.rs"),
            include_str!("index_ui.rs"),
            include_str!("file_system.rs"),
            include_str!("thumbnail.rs"),
        ] {
            for c in source
                .chars()
                .filter(|c| icon_like(*c) && !text.contains(c))
            {
                if !mapped.contains(&c) && !missing.contains(&c) {
                    missing.push(c);
                }
            }
        }
        assert!(
            missing.is_empty(),
            "add these to packaging/icons/map.txt and rebuild the font: {missing:?}"
        );
    }
}
