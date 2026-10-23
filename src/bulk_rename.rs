// Ninimma modification: independent project branding and integration.
//! Bulk rename: turning a set of items and some rules into new names, with
//! every problem found before anything on disk changes.
//!
//! For each item the name (without its extension, unless the rules say
//! otherwise) goes through find and replace, then the template (`{name}`,
//! `{n}`, dates...), then the case change; the extension is put back last.
//! Applying is one all-or-nothing [`file_ops::rename_batch`], undone as one
//! [`Step::Renamed`].

use crate::file_ops::{self, display_name};
use crate::undo::Step;
use chrono::{DateTime, Local, NaiveDateTime};
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::BufReader;
use std::path::{Path, PathBuf};

const DEFAULT_DATE_FORMAT: &str = "%Y-%m-%d";
/// Widest zero padding for `{n}`.
const MAX_DIGITS: usize = 12;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaseChange {
    #[default]
    Unchanged,
    Lower,
    Upper,
    /// Every Word Capitalised.
    Title,
    /// Only the first letter capitalised.
    Sentence,
}

impl CaseChange {
    pub const ALL: [CaseChange; 5] = [
        CaseChange::Unchanged,
        CaseChange::Lower,
        CaseChange::Upper,
        CaseChange::Title,
        CaseChange::Sentence,
    ];

    pub fn label(self) -> &'static str {
        match self {
            CaseChange::Unchanged => "Unchanged",
            CaseChange::Lower => "lowercase",
            CaseChange::Upper => "UPPERCASE",
            CaseChange::Title => "Title Case",
            CaseChange::Sentence => "Sentence case",
        }
    }

    fn apply(self, s: &str) -> String {
        match self {
            CaseChange::Unchanged => s.to_string(),
            CaseChange::Lower => s.to_lowercase(),
            CaseChange::Upper => s.to_uppercase(),
            CaseChange::Title => {
                let mut out = String::with_capacity(s.len());
                let mut start = true;
                for c in s.chars() {
                    if start {
                        out.extend(c.to_uppercase());
                    } else {
                        out.extend(c.to_lowercase());
                    }
                    start = !c.is_alphanumeric() && c != '\'';
                }
                out
            }
            CaseChange::Sentence => {
                let lower = s.to_lowercase();
                let mut chars = lower.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().chain(chars).collect(),
                    None => lower,
                }
            }
        }
    }
}

/// Everything the dialog lets you set; saved as presets.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RenameRules {
    /// The new name: text plus tokens such as `{name}` and `{n}`.
    pub template: String,
    pub find: String,
    pub replace: String,
    pub regex: bool,
    pub match_case: bool,
    pub case: CaseChange,
    pub start: i64,
    pub step: i64,
    /// Minimum digits for `{n}` (zero-padded).
    pub padding: usize,
    /// Rename the extension too (it's then part of `{name}`).
    pub include_extension: bool,
}

impl Default for RenameRules {
    fn default() -> Self {
        Self {
            template: "{name}".to_string(),
            find: String::new(),
            replace: String::new(),
            regex: false,
            match_case: false,
            case: CaseChange::Unchanged,
            start: 1,
            step: 1,
            padding: 0,
            include_extension: false,
        }
    }
}

impl RenameRules {
    /// The classic multi-item F2: "Holiday (1).jpg", "Holiday (2).png"...
    pub fn numbered(stem: &str) -> Self {
        Self {
            template: format!("{} ({{n}})", stem.replace('{', "{{").replace('}', "}}")),
            ..Self::default()
        }
    }
}

/// A preset kept in the settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RenamePreset {
    pub name: String,
    pub rules: RenameRules,
}

/// What a template can refer to, read once when the dialog opens.
#[derive(Clone, Debug)]
pub struct Item {
    pub path: PathBuf,
    pub is_dir: bool,
    pub modified: Option<NaiveDateTime>,
    /// EXIF date taken, for photos that have one.
    pub taken: Option<NaiveDateTime>,
}

impl Item {
    pub fn read(path: &Path) -> Self {
        let meta = fs::symlink_metadata(path).ok();
        let is_dir = meta.as_ref().is_some_and(|m| m.is_dir());
        let modified = meta
            .and_then(|m| m.modified().ok())
            .map(|t| DateTime::<Local>::from(t).naive_local());
        let taken = if is_dir { None } else { date_taken(path) };
        Self {
            path: path.to_path_buf(),
            is_dir,
            modified,
            taken,
        }
    }
}

/// When a photo was taken, from its EXIF data (local time, as cameras store it).
fn date_taken(path: &Path) -> Option<NaiveDateTime> {
    use exif::{In, Reader, Tag, Value};
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    if !matches!(
        ext.as_str(),
        "jpg" | "jpeg" | "tif" | "tiff" | "heic" | "heif" | "avif" | "png" | "webp"
    ) {
        return None;
    }
    let file = fs::File::open(path).ok()?;
    let exif = Reader::new()
        .read_from_container(&mut BufReader::new(file))
        .ok()?;
    let raw = [Tag::DateTimeOriginal, Tag::DateTime]
        .into_iter()
        .find_map(
            |tag| match exif.get_field(tag, In::PRIMARY).map(|f| &f.value) {
                Some(Value::Ascii(v)) => v.first().cloned(),
                _ => None,
            },
        )?;
    let dt = exif::DateTime::from_ascii(&raw).ok()?;
    chrono::NaiveDate::from_ymd_opt(dt.year as i32, dt.month as u32, dt.day as u32)?.and_hms_opt(
        dt.hour as u32,
        dt.minute as u32,
        dt.second as u32,
    )
}

/// One line of the preview.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub from: PathBuf,
    pub new_name: String,
    /// Why this item can't be renamed as shown.
    pub problem: Option<String>,
}

impl Row {
    pub fn old_name(&self) -> String {
        display_name(&self.from)
    }

    pub fn changes(&self) -> bool {
        self.old_name() != self.new_name
    }
}

/// The new names for `items`, in order, with clashes and invalid names
/// flagged. An error means the rules themselves are wrong (a bad regular
/// expression or date format).
pub fn preview(items: &[Item], rules: &RenameRules) -> Result<Vec<Row>, String> {
    let find = if rules.find.is_empty() {
        None
    } else {
        let pattern = if rules.regex {
            rules.find.clone()
        } else {
            regex::escape(&rules.find)
        };
        Some(
            RegexBuilder::new(&pattern)
                .case_insensitive(!rules.match_case)
                .build()
                .map_err(|e| format!("Regular expression: {}", first_line(&e.to_string())))?,
        )
    };
    let mut rows = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let full = display_name(&item.path);
        let (stem, ext) = if rules.include_extension || item.is_dir {
            (full.as_str(), "")
        } else {
            split_extension(&full)
        };
        let stem = match &find {
            // Plain text: `$` in the replacement is just a dollar sign.
            Some(re) if rules.regex => re.replace_all(stem, rules.replace.as_str()).into_owned(),
            Some(re) => re
                .replace_all(stem, regex::NoExpand(&rules.replace))
                .into_owned(),
            None => stem.to_string(),
        };
        let n = rules
            .start
            .saturating_add(rules.step.saturating_mul(i as i64));
        let name = expand(&rules.template, &stem, ext, n, rules.padding, item)?;
        let name = rules.case.apply(name.trim());
        let new_name = format!("{name}{ext}");
        rows.push(Row {
            from: item.path.clone(),
            problem: if name.is_empty() {
                Some("The new name is empty".to_string())
            } else {
                file_ops::validate_file_name(&new_name).err()
            },
            new_name,
        });
    }
    flag_clashes(&mut rows);
    Ok(rows)
}

fn first_line(s: &str) -> &str {
    s.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(s)
        .trim()
}

/// "photo.tar.gz" -> ("photo.tar", ".gz"); ".bashrc" has no extension.
fn split_extension(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name, ""),
    }
}

/// Fill in a template's tokens. `{{` and `}}` are literal braces; unknown
/// tokens are left as typed so they show up in the preview.
fn expand(
    template: &str,
    stem: &str,
    ext: &str,
    n: i64,
    padding: usize,
    item: &Item,
) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = template;
    while let Some(i) = rest.find(['{', '}']) {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        if tail.starts_with("{{") || tail.starts_with("}}") {
            out.push_str(&tail[..1]);
            rest = &tail[2..];
            continue;
        }
        let close = match tail.strip_prefix('{').and_then(|t| t.find('}')) {
            Some(close) => close + 1,
            None => {
                out.push_str(&tail[..1]);
                rest = &tail[1..];
                continue;
            }
        };
        let token = &tail[1..close];
        let (key, arg) = match token.split_once(':') {
            Some((k, a)) => (k, Some(a)),
            None => (token, None),
        };
        match key {
            "name" => out.push_str(stem),
            "ext" => out.push_str(ext.trim_start_matches('.')),
            "n" => {
                // Capped, so a pasted `{n:1000000000}` can't allocate gigabytes.
                let width = arg
                    .and_then(|a| a.parse().ok())
                    .unwrap_or(padding)
                    .min(MAX_DIGITS);
                let digits = format!("{:0width$}", n.unsigned_abs());
                if n < 0 {
                    out.push('-');
                }
                out.push_str(&digits);
            }
            "parent" => out.push_str(&item.path.parent().map(display_name).unwrap_or_default()),
            "modified" | "taken" => {
                // Date taken falls back to the modified date.
                let date = if key == "taken" {
                    item.taken.or(item.modified)
                } else {
                    item.modified
                };
                if let Some(date) = date {
                    out.push_str(&format_date(date, arg.unwrap_or(DEFAULT_DATE_FORMAT))?);
                }
            }
            _ => out.push_str(&tail[..=close]),
        }
        rest = &tail[close + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn format_date(date: NaiveDateTime, format: &str) -> Result<String, String> {
    use chrono::format::{Item as FmtItem, StrftimeItems};
    let items: Vec<FmtItem> = StrftimeItems::new(format).collect();
    if items.iter().any(|i| matches!(i, FmtItem::Error)) {
        return Err(format!(
            "\"{format}\" isn't a date format chrono understands"
        ));
    }
    let mut out = String::new();
    use std::fmt::Write;
    write!(out, "{}", date.format_with_items(items.into_iter()))
        .map_err(|_| format!("\"{format}\" can't be used for this date"))?;
    Ok(out)
}

/// Two items given the same name, or a name taken by something that isn't
/// being renamed away. Never overwrite: these block the whole batch.
fn flag_clashes(rows: &mut [Row]) {
    let key = |name: &str| {
        if cfg!(any(windows, target_os = "macos")) {
            name.to_lowercase()
        } else {
            name.to_string()
        }
    };
    let mut counts: HashMap<(PathBuf, String), usize> = HashMap::new();
    for row in rows.iter() {
        let dir = row.from.parent().unwrap_or(Path::new("")).to_path_buf();
        *counts.entry((dir, key(&row.new_name))).or_default() += 1;
    }
    let sources: Vec<PathBuf> = rows.iter().map(|r| r.from.clone()).collect();
    for row in rows.iter_mut() {
        if row.problem.is_some() || !row.changes() {
            continue;
        }
        let dir = row.from.parent().unwrap_or(Path::new("")).to_path_buf();
        let dest = dir.join(&row.new_name);
        if counts[&(dir, key(&row.new_name))] > 1 {
            row.problem = Some("Another item gets this name too".to_string());
        } else if dest.symlink_metadata().is_ok()
            && !sources
                .iter()
                .any(|s| key(&display_name(s)) == key(&row.new_name) && s.parent() == dest.parent())
            && !file_ops::same_entry(&row.from, &dest)
        {
            row.problem = Some(format!("\"{}\" already exists", row.new_name));
        }
    }
}

/// The renames to do, or why none can happen.
pub fn plan(rows: &[Row]) -> Result<Vec<(PathBuf, PathBuf)>, String> {
    if let Some(bad) = rows.iter().find(|r| r.problem.is_some()) {
        return Err(format!(
            "{}: {}",
            bad.old_name(),
            bad.problem.as_deref().unwrap_or_default()
        ));
    }
    let pairs: Vec<(PathBuf, PathBuf)> = rows
        .iter()
        .filter(|r| r.changes())
        .map(|r| (r.from.clone(), r.from.with_file_name(&r.new_name)))
        .collect();
    if pairs.is_empty() {
        return Err("Nothing would change".to_string());
    }
    Ok(pairs)
}

/// Rename everything the rows say, all or nothing, and return the undo
/// step for it.
pub fn apply(rows: &[Row]) -> Result<Step, String> {
    let pairs = plan(rows)?;
    file_ops::rename_batch(&pairs)?;
    Ok(match <[_; 1]>::try_from(pairs) {
        Ok([(from, to)]) => Step::Moved { from, to },
        Err(pairs) => Step::Renamed { pairs },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn item(path: &str) -> Item {
        Item {
            path: PathBuf::from(path),
            is_dir: false,
            modified: NaiveDate::from_ymd_opt(2026, 3, 4)
                .unwrap()
                .and_hms_opt(5, 6, 7),
            taken: None,
        }
    }

    fn names(items: &[Item], rules: &RenameRules) -> Vec<String> {
        preview(items, rules)
            .unwrap()
            .into_iter()
            .map(|r| r.new_name)
            .collect()
    }

    fn files(dir: &Path, names: &[&str]) -> Vec<Item> {
        names
            .iter()
            .map(|n| {
                let p = dir.join(n);
                fs::write(&p, n).unwrap();
                Item::read(&p)
            })
            .collect()
    }

    #[test]
    fn default_rules_change_nothing() {
        let items = [item("/nowhere/a.jpg"), item("/nowhere/.bashrc")];
        let rows = preview(&items, &RenameRules::default()).unwrap();
        assert!(rows.iter().all(|r| !r.changes() && r.problem.is_none()));
        assert_eq!(plan(&rows), Err("Nothing would change".to_string()));
    }

    #[test]
    fn numbering_with_start_step_and_padding() {
        let items = [item("/x/a.jpg"), item("/x/b.png"), item("/x/c")];
        let rules = RenameRules {
            template: "Trip {n}".into(),
            start: 8,
            step: 2,
            padding: 3,
            ..RenameRules::default()
        };
        assert_eq!(
            names(&items, &rules),
            ["Trip 008.jpg", "Trip 010.png", "Trip 012"]
        );
        let rules = RenameRules {
            template: "{n:2}-{name}".into(),
            start: -1,
            ..RenameRules::default()
        };
        assert_eq!(names(&items, &rules), ["-01-a.jpg", "00-b.png", "01-c"]);
        let huge = RenameRules {
            template: "{n:1000000000}".into(),
            padding: usize::MAX,
            ..RenameRules::default()
        };
        assert_eq!(names(&items[..1], &huge), ["000000000001.jpg"]);
    }

    #[test]
    fn classic_numbered_rename_keeps_extensions() {
        let items = [item("/x/x.jpg"), item("/x/y.png")];
        assert_eq!(
            names(&items, &RenameRules::numbered("Holiday")),
            ["Holiday (1).jpg", "Holiday (2).png"]
        );
        // Braces in the old name are literal.
        assert_eq!(
            names(&items[..1], &RenameRules::numbered("a{n}")),
            ["a{n} (1).jpg"]
        );
    }

    #[test]
    fn find_and_replace_plain_and_regex() {
        let items = [item("/x/IMG_0001.JPG"), item("/x/img_0002.jpg")];
        let plain = RenameRules {
            find: "img_".into(),
            replace: "Photo $1 ".into(),
            ..RenameRules::default()
        };
        assert_eq!(
            names(&items, &plain),
            ["Photo $1 0001.JPG", "Photo $1 0002.jpg"]
        );
        let case = RenameRules {
            match_case: true,
            ..plain
        };
        assert_eq!(names(&items, &case), ["IMG_0001.JPG", "Photo $1 0002.jpg"]);
        let re = RenameRules {
            find: r"^img_0*(\d+)$".into(),
            replace: "Photo ${1}".into(),
            regex: true,
            ..RenameRules::default()
        };
        assert_eq!(names(&items, &re), ["Photo 1.JPG", "Photo 2.jpg"]);
        let bad = RenameRules {
            find: "(".into(),
            regex: true,
            ..RenameRules::default()
        };
        assert!(preview(&items, &bad)
            .unwrap_err()
            .starts_with("Regular expression"));
    }

    #[test]
    fn case_changes_and_extensions() {
        let items = [item("/x/my holiday SNAPS.JPG")];
        let with = |case, include_extension| RenameRules {
            case,
            include_extension,
            ..RenameRules::default()
        };
        assert_eq!(
            names(&items, &with(CaseChange::Lower, false)),
            ["my holiday snaps.JPG"]
        );
        assert_eq!(
            names(&items, &with(CaseChange::Lower, true)),
            ["my holiday snaps.jpg"]
        );
        assert_eq!(
            names(&items, &with(CaseChange::Upper, false)),
            ["MY HOLIDAY SNAPS.JPG"]
        );
        assert_eq!(
            names(&items, &with(CaseChange::Title, false)),
            ["My Holiday Snaps.JPG"]
        );
        assert_eq!(
            names(&items, &with(CaseChange::Sentence, false)),
            ["My holiday snaps.JPG"]
        );
    }

    #[test]
    fn date_tokens() {
        let mut photo = item("/trips/az/p.jpg");
        let items = [photo.clone()];
        let rules = |t: &str| RenameRules {
            template: t.into(),
            ..RenameRules::default()
        };
        assert_eq!(
            names(&items, &rules("{modified} {name}")),
            ["2026-03-04 p.jpg"]
        );
        assert_eq!(
            names(&items, &rules("{modified:%Y%m%d_%H%M%S}")),
            ["20260304_050607.jpg"]
        );
        // No EXIF date: taken falls back to modified.
        assert_eq!(names(&items, &rules("{taken:%b %Y}")), ["Mar 2026.jpg"]);
        photo.taken = NaiveDate::from_ymd_opt(2025, 12, 24)
            .unwrap()
            .and_hms_opt(9, 0, 0);
        assert_eq!(
            names(&[photo], &rules("{taken} {parent} {ext}")),
            ["2025-12-24 az jpg.jpg"]
        );
        assert!(preview(&items, &rules("{modified:%Q}")).is_err());
        // Unknown tokens and stray braces stay as typed.
        assert_eq!(
            names(&items, &rules("{what} {{n}} }")),
            ["{what} {n} }.jpg"]
        );
    }

    #[test]
    fn exif_date_taken_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.jpg");
        fs::write(&path, crate::index::extractor::tests::test_jpeg_with_exif()).unwrap();
        assert_eq!(
            Item::read(&path).taken,
            NaiveDate::from_ymd_opt(2025, 12, 20)
                .unwrap()
                .and_hms_opt(14, 30, 0)
        );
    }

    #[test]
    fn invalid_names_and_clashes_are_flagged() {
        let dir = tempfile::tempdir().unwrap();
        let items = files(dir.path(), &["a.txt", "b.txt", "c.txt"]);
        fs::write(dir.path().join("taken.txt"), "x").unwrap();
        let rows = preview(
            &items,
            &RenameRules {
                find: "^.*$".into(),
                regex: true,
                replace: "same".into(),
                ..RenameRules::default()
            },
        )
        .unwrap();
        assert!(rows
            .iter()
            .all(|r| r.problem.as_deref() == Some("Another item gets this name too")));
        let rows = preview(
            &items[..1],
            &RenameRules {
                template: "taken".into(),
                ..RenameRules::default()
            },
        )
        .unwrap();
        assert_eq!(
            rows[0].problem.as_deref(),
            Some("\"taken.txt\" already exists")
        );
        let rows = preview(
            &items[..1],
            &RenameRules {
                template: "  ".into(),
                ..RenameRules::default()
            },
        )
        .unwrap();
        assert!(rows[0].problem.is_some());
        let rows = preview(
            &items[..1],
            &RenameRules {
                template: "a/b".into(),
                ..RenameRules::default()
            },
        )
        .unwrap();
        assert!(rows[0].problem.is_some());
        assert!(apply(&rows).is_err());
        assert!(dir.path().join("a.txt").exists(), "nothing renamed");
    }

    #[test]
    fn swaps_and_cycles_apply_and_undo_as_one_step() {
        let dir = tempfile::tempdir().unwrap();
        let items = files(dir.path(), &["1.txt", "2.txt", "3.txt"]);
        // 1 -> 2, 2 -> 3, 3 -> 1: every new name is currently taken by the batch.
        let rules = RenameRules {
            template: "{n}".into(),
            start: 2,
            ..RenameRules::default()
        };
        let mut rows = preview(&items, &rules).unwrap();
        rows[2].new_name = "1.txt".into();
        assert!(rows.iter().all(|r| r.problem.is_none()), "{rows:?}");
        let step = apply(&rows).unwrap();
        let read = |n: &str| fs::read_to_string(dir.path().join(n)).unwrap();
        assert_eq!(
            (read("1.txt"), read("2.txt"), read("3.txt")),
            ("3.txt".into(), "1.txt".into(), "2.txt".into())
        );
        let redo = step.undo().unwrap();
        assert_eq!(
            (read("1.txt"), read("2.txt"), read("3.txt")),
            ("1.txt".into(), "2.txt".into(), "3.txt".into())
        );
        redo.undo().unwrap();
        assert_eq!(read("1.txt"), "3.txt");
    }

    #[test]
    fn classic_rename_with_a_selected_item_already_named_like_the_result() {
        let dir = tempfile::tempdir().unwrap();
        // Display order x, y, "Trip (2)": the second new name belongs to the third item.
        let items = files(dir.path(), &["x.jpg", "y.jpg", "Trip (2).jpg"]);
        let rows = preview(&items, &RenameRules::numbered("Trip")).unwrap();
        apply(&rows).unwrap();
        let read = |n: &str| fs::read_to_string(dir.path().join(n)).unwrap();
        assert_eq!(
            (
                read("Trip (1).jpg"),
                read("Trip (2).jpg"),
                read("Trip (3).jpg")
            ),
            ("x.jpg".into(), "y.jpg".into(), "Trip (2).jpg".into())
        );
    }

    #[test]
    fn a_clash_outside_the_batch_leaves_everything_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let items = files(dir.path(), &["a.jpg", "b.jpg"]);
        fs::write(dir.path().join("Trip (2).jpg"), "existing").unwrap();
        let rows = preview(&items, &RenameRules::numbered("Trip")).unwrap();
        assert!(rows[1].problem.is_some());
        assert!(apply(&rows).is_err());
        // Even if the clash appears after the preview, rename_batch refuses.
        let mut rows = rows;
        rows[1].problem = None;
        assert!(apply(&rows).is_err());
        assert!(dir.path().join("a.jpg").exists() && dir.path().join("b.jpg").exists());
        assert_eq!(
            fs::read_to_string(dir.path().join("Trip (2).jpg")).unwrap(),
            "existing"
        );
    }

    #[test]
    fn undo_refuses_when_an_old_name_was_taken_meanwhile() {
        let dir = tempfile::tempdir().unwrap();
        let items = files(dir.path(), &["a.txt", "b.txt"]);
        let rows = preview(
            &items,
            &RenameRules {
                case: CaseChange::Upper,
                ..RenameRules::default()
            },
        )
        .unwrap();
        let step = apply(&rows).unwrap();
        fs::write(dir.path().join("a.txt"), "new").unwrap();
        assert!(step.undo().is_err());
        // Nothing half-undone: B still has its new name, the new file is intact.
        assert!(dir.path().join("B.txt").exists());
        assert_eq!(fs::read_to_string(dir.path().join("a.txt")).unwrap(), "new");
    }

    #[test]
    fn presets_round_trip_and_tolerate_missing_fields() {
        let rules = RenameRules {
            template: "{taken} {n}".into(),
            padding: 2,
            ..RenameRules::default()
        };
        let json = ron::to_string(&RenamePreset {
            name: "Photos".into(),
            rules: rules.clone(),
        })
        .unwrap();
        let back: RenamePreset = ron::from_str(&json).unwrap();
        assert_eq!(back.rules, rules);
        let partial: RenameRules = ron::from_str(r#"(template:"x")"#).unwrap();
        assert_eq!(partial.step, 1);
    }
}
