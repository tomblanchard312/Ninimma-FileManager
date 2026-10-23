// Ninimma modification: independent project branding and integration.
//! Searching the index: plain-language queries, ranked results, and a
//! fresh access check on every result before it's shown.
//!
//! Queries can be written naturally ("receipts from Arizona trip in
//! December"): filler words are dropped, dates become date filters ("in
//! December", "last month", "December 2025"), and words like "photos"
//! narrow the type, on their own too ("photos" lists photos; quote it,
//! "photos", to look for the name). Explicit filters:
//!   ext:pdf,docx          type:image|video|audio|document|archive|code|folder
//!   size:>10MB  size:<1kb modified:today|week|month|year|>2026-01-31|<…
//!   path:photos           "quoted words" are kept exactly, as one phrase
//!
//! Dates use when a photo was taken where known, otherwise when the file
//! was modified.
//!
//! Ranking: names containing every word come first (exact name, then
//! names starting with the first word, then the rest). With full text on,
//! they're followed by files whose text, folder or photo place match, best
//! first (SQLite FTS5 bm25), so a file matching more of the words ranks
//! higher.

use super::db::{descendant_range, native_to_path};
use super::places;
use super::types::{MatchReason, SearchHit, SearchRequest, SearchResults, SearchScope};
use crate::file_system::FileItem;
use chrono::{Datelike, Local, NaiveDate, TimeZone};
use rusqlite::types::Value;
use rusqlite::Connection;
use std::path::PathBuf;

pub const MAX_QUERY_CHARS: usize = 256;
/// Words and filters per query (filler words don't count).
pub const MAX_TERMS: usize = 8;
/// Everything typed, filler included.
pub const MAX_TOKENS: usize = 24;
pub const PAGE_SIZE: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Folder,
    Image,
    Video,
    Audio,
    Document,
    Archive,
    Code,
}

impl Kind {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "folder" | "folders" | "dir" => Kind::Folder,
            "image" | "images" | "picture" | "pictures" | "photo" | "photos" => Kind::Image,
            "video" | "videos" | "movie" | "movies" => Kind::Video,
            "audio" | "music" | "sound" => Kind::Audio,
            "document" | "documents" | "doc" | "docs" => Kind::Document,
            "archive" | "archives" | "zip" => Kind::Archive,
            "code" | "source" => Kind::Code,
            _ => return None,
        })
    }

    fn extensions(self) -> &'static [&'static str] {
        match self {
            Kind::Folder => &[],
            Kind::Image => &[
                "jpg", "jpeg", "png", "gif", "bmp", "webp", "tif", "tiff", "heic", "heif", "avif",
                "svg", "ico", "raw", "cr2", "nef", "arw", "dng",
            ],
            Kind::Video => &[
                "mp4", "mkv", "mov", "avi", "webm", "wmv", "m4v", "mpg", "mpeg", "flv", "3gp",
            ],
            Kind::Audio => &[
                "mp3", "flac", "wav", "ogg", "m4a", "aac", "opus", "wma", "aiff",
            ],
            Kind::Document => &[
                "pdf", "doc", "docx", "odt", "rtf", "txt", "md", "xls", "xlsx", "ods", "csv",
                "ppt", "pptx", "odp", "epub", "pages", "numbers", "key",
            ],
            Kind::Archive => &["zip", "7z", "rar", "tar", "gz", "tgz", "bz2", "xz", "zst"],
            Kind::Code => &[
                "rs", "py", "js", "ts", "tsx", "jsx", "c", "h", "cpp", "hpp", "cs", "java", "kt",
                "go", "rb", "php", "swift", "sh", "toml", "yaml", "yml", "json", "xml", "html",
                "css", "sql",
            ],
        }
    }

    fn name(self) -> &'static str {
        match self {
            Kind::Folder => "folder",
            Kind::Image => "image",
            Kind::Video => "video",
            Kind::Audio => "audio",
            Kind::Document => "document",
            Kind::Archive => "archive",
            Kind::Code => "code",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmp {
    Less,
    Greater,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Query {
    /// Words to find, lowercased (a quoted phrase is one word).
    pub words: Vec<String>,
    /// For each word, other words that mean the same (AZ for Arizona).
    pub alternatives: Vec<Vec<String>>,
    pub extensions: Vec<String>,
    pub kind: Option<Kind>,
    pub size: Option<(Cmp, u64)>,
    /// Dated before/after this time (ns since the epoch).
    pub modified: Option<(Cmp, i64)>,
    /// Dated within [from, to) (ns since the epoch): "last month".
    pub between: Option<(i64, i64)>,
    /// Any of these months (1-12) of any year, unless `year` is set too:
    /// "December" is [12], "summer" is [6, 7, 8].
    pub months: Vec<u32>,
    pub year: Option<i32>,
    /// A span of days in any year, as MMDD (inclusive; may wrap past the
    /// new year): "Christmas" is 1220-1231.
    pub days: Option<(u32, u32)>,
    /// Parts of the sentence that were understood but can't be searched
    /// (e.g. amounts of money), to say so.
    pub not_searchable: Vec<String>,
    /// Words that matter most (picked by the experimental query model):
    /// files matching them rank higher. Never a filter.
    pub emphasis: Vec<String>,
    pub path_words: Vec<String>,
}

const MONTHS: &[&str] = &[
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

/// "december", "dec", "sept". Quote a month ("\"may\"") to search for
/// the word instead.
fn month_number(word: &str) -> Option<u32> {
    const SHORT: &[(&str, u32)] = &[
        ("jan", 1),
        ("feb", 2),
        ("mar", 3),
        ("apr", 4),
        ("jun", 6),
        ("jul", 7),
        ("aug", 8),
        ("sep", 9),
        ("sept", 9),
        ("oct", 10),
        ("nov", 11),
        ("dec", 12),
    ];
    MONTHS
        .iter()
        .position(|m| *m == word)
        .map(|i| i as u32 + 1)
        .or_else(|| SHORT.iter().find(|(s, _)| *s == word).map(|(_, n)| *n))
}

/// Words that carry no meaning for finding files.
const FILLER: &[&str] = &[
    "show", "find", "list", "get", "give", "search", "me", "all", "any", "every", "my", "our",
    "the", "a", "an", "of", "for", "from", "in", "on", "at", "to", "with", "and", "during", "that",
    "which", "were", "was", "is", "are", "i", "we", "taken", "made", "saved", "some", "files",
    "file", "please", "where", "what", "when", "who", "whose", "did", "do", "does", "have", "had",
    "has", "can", "could", "you", "it", "its", "they", "them", "their", "us", "need", "want",
    "looking", "look", "about", "around", "into", "by", "or", "there", "those", "these",
];

/// What a plural type word in a sentence narrows the search to.
enum Narrow {
    Kind(Kind),
    Extensions(&'static [&'static str]),
}

/// File types that type words in a sentence stand for.
const SPREADSHEETS: &[&str] = &[
    "xls", "xlsx", "xlsm", "xlsb", "xlt", "xltx", "xltm", "csv", "tsv", "ods", "ots", "fods",
    "numbers",
];
/// Word-processor and page formats (see Wikipedia's "Document file format").
const DOCUMENTS: &[&str] = &[
    "doc", "docx", "docm", "dot", "dotx", "dotm", "odt", "ott", "fodt", "rtf", "pdf", "txt", "md",
    "pages", "wpd", "wps",
];
const PRESENTATIONS: &[&str] = &[
    "ppt", "pptx", "pptm", "pps", "ppsx", "pot", "potx", "odp", "otp", "key",
];
/// What a receipt is kept as: a PDF, or a photo or scan of one.
const RECEIPTS: &[&str] = &[
    "pdf", "jpg", "jpeg", "jpe", "png", "heic", "heif", "webp", "tif", "tiff", "bmp",
];

/// Words that narrow the type and are still searched for: "receipts" are
/// PDFs and pictures, but not every PDF or picture is a receipt. So are
/// invoices, bills and scans. Common misspellings ("reciept") count too,
/// and are searched for as the word they meant. Returns that word.
fn typed_word(word: &str) -> Option<(&'static str, &'static [&'static str])> {
    let meant = match word {
        "receipt" | "reciept" | "recipt" | "receit" | "reciet" | "reciep" | "receip" => "receipt",
        "receipts" | "reciepts" | "recipts" | "receits" | "reciets" => "receipts",
        "invoice" | "invoce" | "invoise" | "invioce" => "invoice",
        "invoices" | "invoces" | "invoises" | "invioces" => "invoices",
        "scan" => "scan",
        "scans" => "scans",
        "scanned" => "scanned",
        _ => return None,
    };
    Some((meant, RECEIPTS))
}

/// Plural type words, used as filters in a sentence ("photos from Sedona").
fn natural_narrowing(word: &str) -> Option<Narrow> {
    Some(match word {
        // Singular too: "spreadsheet from October" means one of them.
        "documents" | "docs" | "document" => Narrow::Extensions(DOCUMENTS),
        "pdfs" | "pdf" => Narrow::Extensions(&["pdf"]),
        "spreadsheets" | "spreadsheet" => Narrow::Extensions(SPREADSHEETS),
        "presentations" | "presentation" | "slides" | "slideshow" | "slideshows" | "decks"
        | "deck" => Narrow::Extensions(PRESENTATIONS),
        word => Narrow::Kind(natural_kind(word)?),
    })
}

const SEASONS: &[(&str, [u32; 3])] = &[
    ("spring", [3, 4, 5]),
    ("summer", [6, 7, 8]),
    ("fall", [9, 10, 11]),
    ("autumn", [9, 10, 11]),
    ("winter", [12, 1, 2]),
];

/// Holidays as a span of days (MMDD), around the day itself.
const HOLIDAYS: &[(&str, (u32, u32))] = &[
    ("christmas", (1220, 1231)),
    ("xmas", (1220, 1231)),
    ("thanksgiving", (1121, 1130)),
    ("halloween", (1026, 1101)),
    ("valentines", (210, 216)),
    ("valentine's", (210, 216)),
];

const CURRENCY_WORDS: &[&str] = &[
    "dollars", "dollar", "bucks", "usd", "euros", "euro", "eur", "pounds", "gbp",
];
const COMPARISONS: &[&str] = &[
    "over", "under", "above", "below", "more", "less", "than", "least", "most", "about",
];

fn natural_kind(word: &str) -> Option<Kind> {
    Some(match word {
        "photos" | "pictures" | "images" | "pics" | "photo" | "picture" | "image" | "pic" => {
            Kind::Image
        }
        "videos" | "movies" | "clips" | "video" | "movie" | "clip" => Kind::Video,
        "songs" | "music" | "recordings" | "song" | "recording" => Kind::Audio,
        "folders" | "folder" => Kind::Folder,
        _ => return None,
    })
}

impl Query {
    /// The filters in words, to show what was applied.
    pub fn describe_filters(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.extensions.is_empty() {
            out.push(format!("extension: {}", self.extensions.join(", ")));
        }
        if let Some(k) = self.kind {
            out.push(format!("type: {}", k.name()));
        }
        if let Some((cmp, bytes)) = self.size {
            let op = if cmp == Cmp::Greater { "over" } else { "under" };
            out.push(format!(
                "size {op} {}",
                crate::file_system::format_bytes(bytes)
            ));
        }
        let day = |ns: i64| Local.timestamp_nanos(ns).format("%Y-%m-%d").to_string();
        if let Some((cmp, ns)) = self.modified {
            let op = if cmp == Cmp::Greater {
                "since"
            } else {
                "before"
            };
            out.push(format!("dated {op} {}", day(ns)));
        }
        if let Some((from, to)) = self.between {
            out.push(format!("dated {} to {}", day(from), day(to - 1)));
        }
        let month_names = self
            .months
            .iter()
            .map(|m| capitalize(MONTHS[*m as usize - 1]))
            .collect::<Vec<_>>()
            .join("/");
        if let Some((from, to)) = self.days {
            out.push(format!(
                "dated {:02}-{:02} to {:02}-{:02} (any year)",
                from / 100,
                from % 100,
                to / 100,
                to % 100
            ));
        }
        for n in &self.not_searchable {
            out.push(format!("{n} can't be searched yet"));
        }
        match (self.months.is_empty(), self.year) {
            (false, Some(y)) => out.push(format!("dated {month_names} {y}")),
            (false, None) => out.push(format!("dated in {month_names} (any year)")),
            (true, Some(y)) => out.push(format!("dated {y}")),
            (true, None) => {}
        }
        for p in &self.path_words {
            out.push(format!("path contains \"{p}\""));
        }
        out
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// Splits on spaces, keeping "quoted phrases" together (marked quoted).
fn tokens(text: &str) -> Result<Vec<(String, bool)>, String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut was_quoted = false;
    for c in text.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                was_quoted = true;
            }
            c if c.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    out.push((std::mem::take(&mut current), was_quoted));
                }
                was_quoted = false;
            }
            c => current.push(c),
        }
    }
    if quoted {
        return Err("a quote isn't closed".into());
    }
    if !current.is_empty() {
        out.push((current, was_quoted));
    }
    Ok(out)
}

pub fn parse(text: &str) -> Result<Query, String> {
    parse_at(text, Local::now())
}

pub fn parse_at(text: &str, now: chrono::DateTime<Local>) -> Result<Query, String> {
    if text.chars().count() > MAX_QUERY_CHARS {
        return Err(format!(
            "searches can be at most {MAX_QUERY_CHARS} characters"
        ));
    }
    let tokens = tokens(text)?;
    if tokens.len() > MAX_TOKENS {
        return Err(format!("searches can be at most {MAX_TOKENS} words long"));
    }
    let today = now.date_naive();
    let mut q = Query::default();
    let mut kind_words: Vec<(String, Narrow)> = Vec::new();
    let mut typed_extensions: Option<&'static [&'static str]> = None;
    let mut i = 0;
    while i < tokens.len() {
        let (raw, quoted) = (&tokens[i].0, tokens[i].1);
        let lower = raw.to_lowercase();
        // Trimmed like `word` below, so "slide deck?" is still one phrase.
        let next = tokens.get(i + 1).map(|t| {
            t.0.to_lowercase()
                .trim_matches(|c: char| matches!(c, ',' | '.' | '?' | '!' | ';' | '\''))
                .to_string()
        });
        i += 1;
        if quoted {
            q.words.push(lower);
            q.alternatives.push(Vec::new());
            continue;
        }
        if let Some((key, value)) = lower.split_once(':') {
            match key {
                "ext" | "extension" => {
                    for ext in value.split(',').map(|e| e.trim().trim_start_matches('.')) {
                        if ext.is_empty()
                            || ext.len() > 16
                            || !ext
                                .chars()
                                .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
                        {
                            return Err(format!("\"{ext}\" isn't a file extension"));
                        }
                        q.extensions.push(ext.to_string());
                    }
                    continue;
                }
                "type" | "kind" => {
                    q.kind = Some(Kind::parse(value).ok_or_else(|| {
                        format!("unknown type \"{value}\" (try image, video, audio, document, archive, code or folder)")
                    })?);
                    continue;
                }
                "size" => {
                    q.size = Some(parse_size(value)?);
                    continue;
                }
                "modified" | "date" => {
                    q.modified = Some(parse_date(value, now)?);
                    continue;
                }
                "path" | "folder" => {
                    if value.is_empty() {
                        return Err("path: needs some text".into());
                    }
                    q.path_words.push(value.to_string());
                    continue;
                }
                // Not a filter (e.g. a name with a colon): a word.
                _ => {}
            }
        }
        let word = lower.trim_matches(|c: char| matches!(c, ',' | '.' | '?' | '!' | ';' | '\''));
        if word.is_empty() {
            continue;
        }
        // Amounts of money ("$100", "over 100 dollars") can't be searched;
        // they're noted rather than looked for as words.
        let is_money = word.starts_with(['$', '€', '£'])
            || (word.parse::<f64>().is_ok()
                && next.as_deref().is_some_and(|n| CURRENCY_WORDS.contains(&n)));
        if is_money {
            if word.parse::<f64>().is_ok() {
                i += 1; // the currency word
            }
            q.words.retain(|w| !COMPARISONS.contains(&w.as_str()));
            q.alternatives.truncate(q.words.len());
            if !q.not_searchable.iter().any(|n| n == "an amount") {
                q.not_searchable.push("an amount".into());
            }
            continue;
        }
        // "3 weeks ago", "a month ago".
        let count = match word {
            "a" | "an" | "one" => Some(1),
            w => w.parse::<u32>().ok().filter(|n| (1..=500).contains(n)),
        };
        if let (Some(n), Some(unit)) = (count, next.as_deref()) {
            let unit = unit.trim_end_matches('s');
            let then = tokens.get(i + 1).map(|t| t.0.to_lowercase());
            if matches!(unit, "day" | "week" | "month" | "year") && then.as_deref() == Some("ago") {
                q.between = Some(ago(n, unit, today));
                i += 2;
                continue;
            }
        }
        // Dates in words.
        if let Some(month) = month_number(word) {
            q.months = vec![month];
            if let Some(year) = next.as_deref().and_then(year_number) {
                q.year = Some(year);
                i += 1;
            }
            continue;
        }
        if let Some((_, months)) = SEASONS.iter().find(|(name, _)| *name == word) {
            match next.as_deref().and_then(year_number) {
                Some(year) => {
                    q.between = Some(season_range(months, year));
                    i += 1;
                }
                None => q.months = months.to_vec(),
            }
            continue;
        }
        if let Some((_, days)) = holiday(word, next.as_deref()) {
            if next.as_deref().is_some_and(|n| n.starts_with("year")) {
                i += 1; // "new year('s)"
            }
            match tokens
                .get(i)
                .map(|t| t.0.to_lowercase())
                .as_deref()
                .and_then(year_number)
            {
                Some(year) => {
                    q.between = Some(days_range(days, year));
                    i += 1;
                }
                None => q.days = Some(days),
            }
            continue;
        }
        match (word, next.as_deref()) {
            ("today", _) => {
                q.between = Some(day_range(today, today));
                continue;
            }
            ("yesterday", _) => {
                let y = today - chrono::Days::new(1);
                q.between = Some(day_range(y, y));
                continue;
            }
            ("this" | "last" | "past", Some(unit @ ("week" | "month" | "year"))) => {
                q.between = Some(period(word != "this", unit, today));
                i += 1;
                continue;
            }
            ("this" | "last", Some(next_word)) => {
                // "last summer", "this winter", "last Christmas".
                if let Some((_, months)) = SEASONS.iter().find(|(name, _)| *name == next_word) {
                    q.between = Some(recent_season(months, word == "last", today));
                    i += 1;
                    continue;
                }
                let after = tokens.get(i + 1).map(|t| t.0.to_lowercase());
                if let Some((_, days)) = holiday(next_word, after.as_deref()) {
                    q.between = Some(recent_days(days, word == "last", today));
                    i += if after.as_deref().is_some_and(|a| a.starts_with("year")) {
                        2
                    } else {
                        1
                    };
                    continue;
                }
            }
            ("past", Some(n)) => {
                // "past 3 weeks"
                let unit = tokens.get(i + 1).map(|t| t.0.to_lowercase());
                if let (Ok(n), Some(unit)) = (n.parse::<u32>(), unit) {
                    let unit = unit.trim_end_matches('s').to_string();
                    if (1..=500).contains(&n)
                        && matches!(unit.as_str(), "day" | "week" | "month" | "year")
                    {
                        q.between = Some((
                            start_of(back(today, n, &unit)),
                            start_of(today + chrono::Days::new(1)),
                        ));
                        i += 2;
                        continue;
                    }
                }
            }
            _ => {}
        }
        if let Some(year) = year_number(word) {
            // A year after "in"/"from"/"during" is a date; otherwise it may
            // be part of a name ("report 2025").
            let prev = i.checked_sub(2).map(|p| tokens[p].0.to_lowercase());
            if matches!(prev.as_deref(), Some("in" | "from" | "during" | "of")) {
                q.year = Some(year);
                continue;
            }
        }
        if FILLER.contains(&word) {
            continue;
        }
        // "slide show", "slide deck": one type word in two.
        if word == "slide" && matches!(next.as_deref(), Some("show" | "shows" | "deck" | "decks")) {
            i += 1;
            kind_words.push(("slide show".into(), Narrow::Extensions(PRESENTATIONS)));
            continue;
        }
        if let Some(narrow) = natural_narrowing(word) {
            kind_words.push((word.to_string(), narrow));
            continue;
        }
        if let Some((meant, exts)) = typed_word(word) {
            // Applied after the loop, so an explicit ext: filter wins
            // wherever it's written.
            typed_extensions.get_or_insert(exts);
            q.alternatives.push(places::synonyms(meant));
            q.words.push(meant.to_string());
            continue;
        }
        q.alternatives.push(places::synonyms(
            raw.trim_matches(|c: char| !c.is_alphanumeric()),
        ));
        q.words.push(word.to_string());
    }
    // Type words are filters, alone too: "photos" lists photos (not code
    // that mentions images), "spreadsheets" lists spreadsheets. Quote one
    // to look for the name: "photos".
    if let Some(exts) = typed_extensions.filter(|_| q.extensions.is_empty()) {
        q.extensions = exts.iter().map(|e| e.to_string()).collect();
    }
    for (word, narrow) in kind_words {
        match narrow {
            Narrow::Kind(kind) if q.kind.is_none() => q.kind = Some(kind),
            Narrow::Extensions(exts) if q.extensions.is_empty() => {
                q.extensions = exts.iter().map(|e| e.to_string()).collect();
            }
            _ => {
                q.words.push(word);
                q.alternatives.push(Vec::new());
            }
        }
    }
    if q.words.len() + q.extensions.len().min(1) + q.path_words.len() > MAX_TERMS {
        return Err(format!(
            "searches can have at most {MAX_TERMS} words and filters"
        ));
    }
    Ok(q)
}

fn year_number(word: &str) -> Option<i32> {
    let y: i32 = word.parse().ok()?;
    (word.len() == 4 && (1970..=2100).contains(&y)).then_some(y)
}

fn start_of(d: NaiveDate) -> i64 {
    Local
        .from_local_datetime(&d.and_hms_opt(0, 0, 0).expect("midnight"))
        .earliest()
        .and_then(|t| t.timestamp_nanos_opt())
        .unwrap_or(0)
}

/// [first day 00:00, the day after `last` 00:00)
fn day_range(first: NaiveDate, last: NaiveDate) -> (i64, i64) {
    (start_of(first), start_of(last + chrono::Days::new(1)))
}

/// This or last week/month/year, as a range.
fn period(last: bool, unit: &str, today: NaiveDate) -> (i64, i64) {
    match unit {
        "week" => {
            let monday = today - chrono::Days::new(today.weekday().num_days_from_monday() as u64);
            let start = if last {
                monday - chrono::Days::new(7)
            } else {
                monday
            };
            (start_of(start), start_of(start + chrono::Days::new(7)))
        }
        "month" => {
            let first = today.with_day(1).expect("day 1");
            let start = if last {
                first - chrono::Months::new(1)
            } else {
                first
            };
            (start_of(start), start_of(start + chrono::Months::new(1)))
        }
        _ => {
            let first = today.with_ordinal(1).expect("day 1");
            let start = if last {
                first - chrono::Months::new(12)
            } else {
                first
            };
            (start_of(start), start_of(start + chrono::Months::new(12)))
        }
    }
}

/// `n` days/weeks/months/years before `today`.
fn back(today: NaiveDate, n: u32, unit: &str) -> NaiveDate {
    match unit {
        "day" => today - chrono::Days::new(n as u64),
        "week" => today - chrono::Days::new(7 * n as u64),
        "month" => today - chrono::Months::new(n),
        _ => today - chrono::Months::new(12 * n),
    }
}

/// "3 weeks ago": that day, week, month or year.
fn ago(n: u32, unit: &str, today: NaiveDate) -> (i64, i64) {
    let then = back(today, n, unit);
    match unit {
        "day" => day_range(then, then),
        "week" => {
            let monday = then - chrono::Days::new(then.weekday().num_days_from_monday() as u64);
            (start_of(monday), start_of(monday + chrono::Days::new(7)))
        }
        "month" => {
            let first = then.with_day(1).expect("day 1");
            (start_of(first), start_of(first + chrono::Months::new(1)))
        }
        _ => {
            let first = then.with_ordinal(1).expect("day 1");
            (start_of(first), start_of(first + chrono::Months::new(12)))
        }
    }
}

/// A season of a given year; winter is December of that year to February.
fn season_range(months: &[u32; 3], year: i32) -> (i64, i64) {
    let first = NaiveDate::from_ymd_opt(year, months[0], 1).expect("valid month");
    (start_of(first), start_of(first + chrono::Months::new(3)))
}

/// The latest season that has ended ("last summer"), or this year's
/// ("this summer").
fn recent_season(months: &[u32; 3], last: bool, today: NaiveDate) -> (i64, i64) {
    let mut year = today.year();
    if months[0] == 12 && today.month() <= 2 {
        year -= 1; // this winter began last December
    }
    let (start, end) = season_range(months, year);
    if last && end > start_of(today) {
        season_range(months, year - 1)
    } else {
        (start, end)
    }
}

/// A holiday name, with "new year('s)" spread over two words.
fn holiday<'a>(word: &str, next: Option<&str>) -> Option<(&'a str, (u32, u32))> {
    if word == "new" && next.is_some_and(|n| matches!(n, "year" | "years" | "year's")) {
        return Some(("new year", (1228, 103)));
    }
    HOLIDAYS
        .iter()
        .find(|(name, _)| *name == word)
        .map(|(n, d)| (*n, *d))
}

/// A span of days (MMDD) in a given year; one that wraps ends next year.
fn days_range((from, to): (u32, u32), year: i32) -> (i64, i64) {
    let start = NaiveDate::from_ymd_opt(year, from / 100, from % 100).expect("valid day");
    let end_year = if to < from { year + 1 } else { year };
    let end = NaiveDate::from_ymd_opt(end_year, to / 100, to % 100).expect("valid day");
    day_range(start, end)
}

/// The latest past (or this year's) occurrence of a span of days.
fn recent_days(days: (u32, u32), last: bool, today: NaiveDate) -> (i64, i64) {
    let mut year = today.year();
    if days.1 < days.0 && today.month() == 1 {
        year -= 1;
    }
    let range = days_range(days, year);
    if last && range.1 > start_of(today) {
        days_range(days, year - 1)
    } else {
        range
    }
}

fn parse_cmp(value: &str) -> Result<(Cmp, &str), String> {
    if let Some(rest) = value.strip_prefix(">=").or_else(|| value.strip_prefix('>')) {
        Ok((Cmp::Greater, rest))
    } else if let Some(rest) = value.strip_prefix("<=").or_else(|| value.strip_prefix('<')) {
        Ok((Cmp::Less, rest))
    } else {
        Err(format!("\"{value}\" needs > or < in front"))
    }
}

fn parse_size(value: &str) -> Result<(Cmp, u64), String> {
    let (cmp, rest) = parse_cmp(value)?;
    let split = rest
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(rest.len());
    let (number, unit) = rest.split_at(split);
    let number: f64 = number
        .parse()
        .map_err(|_| format!("\"{rest}\" isn't a size"))?;
    let multiplier: u64 = match unit {
        "" | "b" => 1,
        "k" | "kb" => 1 << 10,
        "m" | "mb" => 1 << 20,
        "g" | "gb" => 1 << 30,
        "t" | "tb" => 1 << 40,
        _ => {
            return Err(format!(
                "unknown size unit \"{unit}\" (use KB, MB, GB or TB)"
            ))
        }
    };
    let bytes = number * multiplier as f64;
    if !(0.0..=(1u64 << 60) as f64).contains(&bytes) {
        return Err(format!("\"{rest}\" is out of range"));
    }
    Ok((cmp, bytes as u64))
}

fn parse_date(value: &str, now: chrono::DateTime<Local>) -> Result<(Cmp, i64), String> {
    let today = now.date_naive();
    let relative = match value {
        "today" => Some(today),
        "week" | "thisweek" => {
            Some(today - chrono::Days::new(today.weekday().num_days_from_monday() as u64))
        }
        "month" | "thismonth" => today.with_day(1),
        "year" | "thisyear" => today.with_ordinal(1),
        _ => None,
    };
    if let Some(day) = relative {
        return Ok((Cmp::Greater, start_of(day)));
    }
    let (cmp, rest) = parse_cmp(value)?;
    let day = NaiveDate::parse_from_str(rest, "%Y-%m-%d").map_err(|_| {
        format!("\"{rest}\" isn't a date (use YYYY-MM-DD, today, week, month or year)")
    })?;
    if !(1900..=2200).contains(&day.year()) {
        return Err(format!("\"{rest}\" is out of range"));
    }
    Ok((cmp, start_of(day)))
}

/// When a file is dated: when a photo was taken, else when last modified.
const DATED: &str = "COALESCE(f.taken_ns, f.modified_ns)";

/// The WHERE conditions for scope and filters (not words), on `files f`.
fn filter_sql(
    request: &SearchRequest,
    query: &Query,
    bind: &mut dyn FnMut(Value) -> String,
) -> String {
    let mut sql = String::new();
    if request.scope == SearchScope::Subfolders {
        let (low, high) = descendant_range(&request.folder);
        let (low, high) = (bind(Value::Blob(low)), bind(Value::Blob(high)));
        sql.push_str(&format!(
            " AND f.native_path >= {low} AND f.native_path < {high}"
        ));
    }
    for p in &query.path_words {
        // lower() folds ASCII only; names are folded fully when indexed.
        sql.push_str(&format!(
            " AND instr(lower(f.display_path), {}) > 0",
            bind(Value::Text(p.clone()))
        ));
    }
    let mut extensions: Vec<&str> = query.extensions.iter().map(String::as_str).collect();
    match query.kind {
        Some(Kind::Folder) => sql.push_str(" AND f.is_dir = 1"),
        Some(kind) => extensions.extend(kind.extensions()),
        None => {}
    }
    if !extensions.is_empty() {
        let list: Vec<String> = extensions
            .iter()
            .map(|e| bind(Value::Text(e.to_string())))
            .collect();
        sql.push_str(&format!(" AND f.extension IN ({})", list.join(",")));
    }
    if let Some((cmp, bytes)) = query.size {
        let op = if cmp == Cmp::Greater { ">" } else { "<" };
        sql.push_str(&format!(
            " AND f.is_dir = 0 AND f.size_bytes {op} {}",
            bind(Value::Integer(bytes as i64))
        ));
    }
    if let Some((cmp, ns)) = query.modified {
        let op = if cmp == Cmp::Greater { ">=" } else { "<" };
        sql.push_str(&format!(" AND {DATED} {op} {}", bind(Value::Integer(ns))));
    }
    if let Some((from, to)) = query.between {
        let (from, to) = (bind(Value::Integer(from)), bind(Value::Integer(to)));
        sql.push_str(&format!(" AND {DATED} >= {from} AND {DATED} < {to}"));
    }
    let local = |fmt: &str| {
        format!(
            "CAST(strftime('{fmt}', {DATED} / 1000000000, 'unixepoch', 'localtime') AS INTEGER)"
        )
    };
    if !query.months.is_empty() {
        let list: Vec<String> = query
            .months
            .iter()
            .map(|m| bind(Value::Integer(*m as i64)))
            .collect();
        sql.push_str(&format!(" AND {} IN ({})", local("%m"), list.join(",")));
    }
    if let Some((from, to)) = query.days {
        let day = local("%m%d");
        let (from, to) = (
            bind(Value::Integer(from as i64)),
            bind(Value::Integer(to as i64)),
        );
        // A span past the new year (Dec 28 - Jan 3) wraps.
        sql.push_str(&format!(
            " AND CASE WHEN {from} <= {to} THEN {day} BETWEEN {from} AND {to} ELSE ({day} >= {from} OR {day} <= {to}) END"
        ));
    }
    if let Some(y) = query.year {
        sql.push_str(&format!(
            " AND {} = {}",
            local("%Y"),
            bind(Value::Integer(y as i64))
        ));
    }
    sql
}

/// An FTS5 query: every word (or one of its alternatives), each quoted
/// so nothing typed is read as FTS syntax. "Fake Street" finds files with
/// both words, not every file with either. Type words ("receipt") already
/// narrow the file type, so they aren't required in the text too, unless
/// nothing else was typed. With `any`, any of the words: used only when
/// no file has them all (a loose sentence: "the contract with acme").
fn fts_match(query: &Query, any: bool) -> String {
    fts_terms(query, any)
}

/// The typed words (not type words) as one phrase, in order: "Example LLC"
/// finds that phrase, not every file with "example" somewhere. None for a
/// single word.
fn fts_phrase(query: &Query) -> Option<String> {
    let words: Vec<String> = query
        .words
        .iter()
        .filter(|w| typed_word(w).is_none())
        .map(|w| w.replace('"', "\"\""))
        .collect();
    (words.len() > 1).then(|| format!("\"{}\"", words.join(" ")))
}

fn fts_terms(query: &Query, any: bool) -> String {
    let quote = |w: &str| format!("\"{}\"", w.replace('"', "\"\""));
    let terms: Vec<(bool, String)> = query
        .words
        .iter()
        .zip(&query.alternatives)
        .map(|(w, alts)| {
            let mut terms = vec![quote(w)];
            terms.extend(alts.iter().map(|a| quote(a)));
            (typed_word(w).is_some(), format!("({})", terms.join(" OR ")))
        })
        .collect();
    let required: Vec<&str> = terms
        .iter()
        .filter(|(typed, _)| !typed)
        .map(|(_, t)| t.as_str())
        .collect();
    if any || required.is_empty() {
        terms
            .iter()
            .map(|(_, t)| t.as_str())
            .collect::<Vec<_>>()
            .join(" OR ")
    } else {
        required.join(" AND ")
    }
}

/// A full-text match: file id, path bytes, text excerpt, place.
type FtsRow = (i64, Vec<u8>, String, Option<String>);

/// Combines the name/text candidates with meaning matches by score. A
/// file scores its similarity to the search (just under the floor if it
/// has none), plus a small bonus for the share of the typed words it
/// contains. Name matches stay on top. Scores, not ranks: with rank fusion
/// a file that merely shared a word ("receipt" in a hardware-store
/// receipt) beat the best meaning match ("gas station receipt" → a fuel
/// receipt), because being in both lists counted more than how close it
/// was (measured in `tests/eval.rs`).
fn fuse(candidates: Vec<Candidate>, similar: &[(Vec<u8>, f32, MatchReason)]) -> Vec<Candidate> {
    let boost = |r: &MatchReason| match r {
        MatchReason::ExactName => 3.0,
        MatchReason::NameStartsWith => 2.0,
        MatchReason::NameContains => 1.0,
        _ => 0.0,
    };
    let meaning: std::collections::HashMap<&[u8], f32> =
        similar.iter().map(|(n, s, _)| (n.as_slice(), *s)).collect();
    let mut scored: Vec<(f32, usize, Candidate)> = Vec::new();
    for (order, c) in candidates.into_iter().enumerate() {
        let similarity = meaning
            .get(c.native.as_slice())
            .copied()
            .unwrap_or(NO_MEANING);
        let score = boost(&c.reason) + similarity + WORD_BONUS * c.words;
        scored.push((score, order, c));
    }
    for (native, similarity, reason) in similar {
        if !scored.iter().any(|(_, _, c)| c.native == *native) {
            let order = scored.len();
            scored.push((
                *similarity,
                order,
                Candidate {
                    native: native.clone(),
                    reason: reason.clone(),
                    words: 0.0,
                },
            ));
        }
    }
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, _, c)| c).collect()
}

/// One result before its access check.
struct Candidate {
    native: Vec<u8>,
    reason: MatchReason,
    /// The share of the typed words it contains (0 to 1).
    words: f32,
}

/// Runs a search. `cancelled` is checked while it runs; a newer search
/// makes it stop early (the caller drops the partial result).
/// Meaning matches below this similarity are ignored. Measured with BGE
/// small on the test documents (`tests/eval.rs`): unrelated text scores
/// 0.40-0.50, and right answers 0.52-0.76 (a single word like "receipts"
/// scores lower than a sentence), so a higher floor hid real matches.
pub const MEANING_FLOOR: f32 = 0.5;
/// ... and so are those far below the best one.
const MEANING_SPREAD: f32 = 0.08;
/// The similarity a word match without a meaning match is given.
const NO_MEANING: f32 = MEANING_FLOOR - 0.05;
/// Added for containing every typed word (a share of it for some).
const WORD_BONUS: f32 = 0.05;

/// Whether a result is of the type the search asked for (a type word or
/// type:, ext: or a type word like "spreadsheets"), by its extension.
fn of_wanted_type(query: &Query, path: &std::path::Path, is_dir: bool) -> bool {
    let mut wanted: Vec<&str> = query.extensions.iter().map(String::as_str).collect();
    match query.kind {
        Some(Kind::Folder) => return is_dir,
        Some(kind) => wanted.extend(kind.extensions()),
        None => {}
    }
    if wanted.is_empty() {
        return true;
    }
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    !is_dir && wanted.contains(&ext.as_str())
}

/// At most this many picture matches are considered per search.
const MAX_PICTURES: usize = 500;

/// A picture match on the meaning scale: the floor ranks just above a
/// weak text match, and clear pictures (0.06+) beside good ones.
fn picture_similarity(score: f32) -> f32 {
    MEANING_FLOOR + 0.05 + 2.5 * (score - crate::pictures::MATCH_FLOOR)
}

/// Runs a search. `meaning` holds smart-search hits (file id, the chunk's
/// text hash, similarity), best first; empty without smart search.
/// `pictures` holds photos and videos showing what was searched for (file
/// id, score), best first.
/// `cancelled` is checked while it runs; a newer search makes it stop
/// early (the caller drops the partial result).
pub fn run(
    conn: &Connection,
    request: &SearchRequest,
    query: &Query,
    meaning: &[(i64, Vec<u8>, f32)],
    pictures: &[(i64, f32)],
    cancelled: &dyn Fn() -> bool,
) -> rusqlite::Result<(SearchResults, Vec<PathBuf>)> {
    let wanted = request.offset + request.limit + 1;
    let first = query.words.first().cloned().unwrap_or_default();

    // 1. Names containing every word.
    let mut args: Vec<Value> = Vec::new();
    let mut bind = |v: Value| {
        args.push(v);
        format!("?{}", args.len())
    };
    let mut sql = String::from("SELECT f.native_path, f.filename_folded FROM files f WHERE 1");
    for w in &query.words {
        sql.push_str(&format!(
            " AND instr(f.filename_folded, {}) > 0",
            bind(Value::Text(w.clone()))
        ));
    }
    sql.push_str(&filter_sql(request, query, &mut bind));
    if first.is_empty() {
        sql.push_str(&format!(
            " ORDER BY f.is_dir DESC, {DATED} DESC, f.filename_folded"
        ));
    } else {
        let word = bind(Value::Text(first.clone()));
        let stem = bind(Value::Text(format!("{first}.")));
        sql.push_str(&format!(
            " ORDER BY CASE WHEN f.filename_folded = {word} OR substr(f.filename_folded, 1, length({stem})) = {stem} THEN 0 \
               WHEN substr(f.filename_folded, 1, length({word})) = {word} THEN 1 ELSE 2 END, \
               f.is_dir DESC, length(f.filename_folded), f.filename_folded"
        ));
    }
    sql.push_str(&format!(" LIMIT {}", bind(Value::Integer(wanted as i64))));
    let mut candidates: Vec<Candidate> = conn
        .prepare(&sql)?
        .query_map(rusqlite::params_from_iter(args.iter()), |r| {
            Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, String>(1)?))
        })?
        .map(|row| {
            row.map(|(native, folded)| {
                let reason = if first.is_empty() {
                    MatchReason::Filters
                } else if folded == first || folded.starts_with(&format!("{first}.")) {
                    MatchReason::ExactName
                } else if folded.starts_with(&first) {
                    MatchReason::NameStartsWith
                } else {
                    MatchReason::NameContains
                };
                Candidate {
                    native,
                    reason,
                    words: 1.0,
                }
            })
        })
        .collect::<Result<_, _>>()?;

    // 2. With full text: text, folders and places matching every word
    // (any word, if no file has them all).
    if request.full_text && !query.words.is_empty() && !cancelled() {
        let fetch = |matcher: String| -> rusqlite::Result<Vec<FtsRow>> {
            let mut args: Vec<Value> = Vec::new();
            let mut bind = |v: Value| {
                args.push(v);
                format!("?{}", args.len())
            };
            let matcher = bind(Value::Text(matcher));
            let mut sql = format!(
                "SELECT f.id, f.native_path, snippet(file_fts, 3, '«', '»', '…', 10), f.place
                 FROM file_fts JOIN files f ON f.id = file_fts.rowid
                 WHERE file_fts MATCH {matcher}"
            );
            sql.push_str(&filter_sql(request, query, &mut bind));
            // bm25 weights: name 10, folder 3, place 3, text 1. A wider pool
            // than one page, re-ranked below by how many words each matches.
            sql.push_str(&format!(
                " ORDER BY bm25(file_fts, 10.0, 3.0, 3.0, 1.0) LIMIT {}",
                bind(Value::Integer((wanted * 4).max(400) as i64))
            ));
            conn.prepare(&sql)?
                .query_map(rusqlite::params_from_iter(args.iter()), |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })?
                .collect()
        };
        // The exact phrase if any file has it, else every word, else any.
        let mut found = match fts_phrase(query) {
            Some(phrase) => fetch(phrase)?,
            None => Vec::new(),
        };
        if found.is_empty() {
            found = fetch(fts_match(query, false))?;
        }
        if found.is_empty() && candidates.is_empty() && query.words.len() > 1 {
            found = fetch(fts_match(query, true))?;
        }

        // How many of the asked-for words each file matches (a word counts
        // once, whichever of its alternatives matched), so "receipts from
        // Arizona" ranks a receipt from Arizona above things matching only
        // one of them. bm25 order breaks ties.
        let ids: Vec<i64> = found.iter().map(|f| f.0).collect();
        let mut coverage: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
        if query.words.len() > 1 && !ids.is_empty() && !cancelled() {
            let id_list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
            for (word, alts) in query.words.iter().zip(&query.alternatives) {
                let one = Query {
                    words: vec![word.clone()],
                    alternatives: vec![alts.clone()],
                    ..Query::default()
                };
                let mut stmt = conn.prepare_cached(&format!(
                    "SELECT rowid FROM file_fts WHERE file_fts MATCH ?1 AND rowid IN ({id_list})"
                ))?;
                // Emphasised words count double.
                let weight = if query.emphasis.contains(word) { 2 } else { 1 };
                for id in stmt.query_map([fts_match(&one, false)], |r| r.get::<_, i64>(0))? {
                    *coverage.entry(id?).or_default() += weight;
                }
            }
        }
        let mut found: Vec<(usize, FtsRow)> = found.into_iter().enumerate().collect();
        found.sort_by_key(|(rank, f)| {
            (
                std::cmp::Reverse(coverage.get(&f.0).copied().unwrap_or(1)),
                *rank,
            )
        });

        let words: Vec<&str> = query
            .words
            .iter()
            .chain(query.alternatives.iter().flatten())
            .map(String::as_str)
            .collect();
        let most = (query.words.len() + query.emphasis.len()).max(1) as f32;
        for (_, (id, native, snippet, place)) in found {
            if candidates.iter().any(|c| c.native == native) {
                continue;
            }
            let reason = if snippet.contains('«') {
                MatchReason::Content(snippet.replace(['\n', '\r', '\t'], " "))
            } else if let Some(place) = place.filter(|p| {
                let p = p.to_lowercase();
                words.iter().any(|w| p.contains(*w))
            }) {
                MatchReason::Place(place)
            } else {
                MatchReason::Folder
            };
            let words = match coverage.get(&id) {
                Some(n) => (*n as f32 / most).min(1.0),
                None if query.words.len() > 1 => 1.0 / most,
                None => 1.0,
            };
            candidates.push(Candidate {
                native,
                reason,
                words,
            });
            if candidates.len() >= wanted {
                break;
            }
        }
    }

    // 3. Smart search: similar in meaning, and pictures showing what was
    // searched for. Only vectors made from a file's current text count,
    // and the same filters apply.
    if (!meaning.is_empty() || !pictures.is_empty()) && !cancelled() {
        let best = meaning.first().map_or(0.0, |m| m.2);
        let close: Vec<&(i64, Vec<u8>, f32)> = meaning
            .iter()
            .filter(|m| m.2 >= MEANING_FLOOR && m.2 >= best - MEANING_SPREAD)
            .collect();
        let pictures: Vec<&(i64, f32)> = pictures.iter().take(MAX_PICTURES).collect();
        let mut similar: Vec<(Vec<u8>, f32, MatchReason)> = Vec::new();
        if !close.is_empty() || !pictures.is_empty() {
            let mut args: Vec<Value> = Vec::new();
            let mut bind = |v: Value| {
                args.push(v);
                format!("?{}", args.len())
            };
            let ids: Vec<String> = close
                .iter()
                .map(|m| m.0)
                .chain(pictures.iter().map(|p| p.0))
                .map(|id| bind(Value::Integer(id)))
                .collect();
            let mut sql = format!(
                "SELECT f.id, f.native_path FROM files f WHERE f.id IN ({})",
                ids.join(",")
            );
            sql.push_str(&filter_sql(request, query, &mut bind));
            let allowed: std::collections::HashMap<i64, Vec<u8>> = conn
                .prepare(&sql)?
                .query_map(rusqlite::params_from_iter(args.iter()), |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?
                .collect::<Result<_, _>>()?;
            for (file_id, hash, score) in close {
                let current = super::db::chunk_is_current(conn, *file_id, hash).unwrap_or(false);
                if let (true, Some(native)) = (current, allowed.get(file_id)) {
                    similar.push((native.clone(), *score, MatchReason::Meaning(*score)));
                }
            }
            for (file_id, score) in pictures {
                if let Some(native) = allowed.get(file_id) {
                    let similarity = picture_similarity(*score);
                    match similar.iter_mut().find(|s| s.0 == *native) {
                        Some(s) if s.1 >= similarity => {}
                        Some(s) => *s = (native.clone(), similarity, MatchReason::Picture),
                        None => similar.push((native.clone(), similarity, MatchReason::Picture)),
                    }
                }
            }
            similar.sort_by(|a, b| b.1.total_cmp(&a.1));
        }
        if !similar.is_empty() {
            candidates = fuse(candidates, &similar);
        }
    }

    let more = candidates.len() > request.offset + request.limit;
    let mut hits = Vec::new();
    let mut stale = Vec::new();
    for c in candidates
        .into_iter()
        .skip(request.offset)
        .take(request.limit)
    {
        if cancelled() {
            break;
        }
        let path = native_to_path(&c.native);
        // Checked again now: deleted, moved or no longer readable files are
        // dropped (and forgotten), never shown.
        let item =
            FileItem::new(path.clone()).filter(|i| super::scanner::readable(&i.path, i.is_dir));
        // Whatever found it, a "photo" search shows only photos, a "video"
        // search only videos (and so on): checked once more here.
        if item
            .as_ref()
            .is_some_and(|i| !of_wanted_type(query, &i.path, i.is_dir))
        {
            continue;
        }
        match item {
            Some(item) => hits.push(SearchHit {
                item,
                reason: c.reason,
            }),
            None => stale.push(path),
        }
    }
    let results = SearchResults {
        generation: request.generation,
        offset: request.offset,
        hits,
        more,
        filters: query.describe_filters(),
        refined: false,
    };
    Ok((results, stale))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::config::{Exclusions, IndexSettings};
    use crate::index::db::tests::open_test_db;
    use crate::index::scanner::{scan_root, ScanControl};
    use crate::security::secret::{SecretBytes, KEY_LEN};
    use std::fs;
    use std::path::Path;
    use std::time::Duration;

    fn now() -> chrono::DateTime<Local> {
        Local.with_ymd_and_hms(2026, 10, 15, 12, 0, 0).unwrap()
    }

    #[test]
    fn words_and_filters_parse() {
        let q = parse_at(
            "Tax report ext:pdf,.docx size:>1.5MB modified:>2026-01-31 path:Finance",
            now(),
        )
        .unwrap();
        assert_eq!(q.words, ["tax", "report"]);
        assert_eq!(q.extensions, ["pdf", "docx"]);
        assert_eq!(q.size, Some((Cmp::Greater, (1.5 * 1048576.0) as u64)));
        assert!(matches!(q.modified, Some((Cmp::Greater, _))));
        assert_eq!(q.path_words, ["finance"]);
        let q = parse_at("\"annual report\" type:images", now()).unwrap();
        assert_eq!(q.words, ["annual report"]);
        assert_eq!(q.kind, Some(Kind::Image));
        // An unknown key is just part of a name.
        assert_eq!(parse_at("c:drive", now()).unwrap().words, ["c:drive"]);
    }

    #[test]
    fn malformed_and_unreasonable_queries_are_rejected() {
        for bad in [
            "size:10MB",
            "size:>10parsecs",
            "size:>99999999TB",
            "type:spreadsheet-ish",
            "modified:>yesterdayish",
            "modified:>0001-01-01",
            "ext:p/d/f",
            "\"unclosed",
            "path:",
            "w1 w2 w3 w4 w5 w6 w7 w8 w9",
        ] {
            assert!(parse_at(bad, now()).is_err(), "{bad} should be rejected");
        }
        assert!(parse_at(&"x".repeat(MAX_QUERY_CHARS + 1), now()).is_err());
        assert!(parse_at(&"the ".repeat(MAX_TOKENS + 1), now()).is_err());
    }

    #[test]
    fn plain_language_queries_become_words_and_filters() {
        let q = parse_at("Show all receipts from Arizona trip on December", now()).unwrap();
        assert_eq!(q.words, ["receipts", "arizona", "trip"]);
        assert_eq!(q.alternatives, [vec![], vec!["az".to_string()], vec![]]);
        assert_eq!((q.months.clone(), q.year), (vec![12], None));

        let q = parse_at("photos from Sedona last month", now()).unwrap();
        assert_eq!(q.kind, Some(Kind::Image));
        assert_eq!(q.words, ["sedona"]);
        let (from, to) = q.between.unwrap();
        let september = Local.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap();
        let october = Local.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
        assert_eq!(
            (from, to),
            (
                september.timestamp_nanos_opt().unwrap(),
                october.timestamp_nanos_opt().unwrap()
            )
        );

        let q = parse_at("tax documents December 2025", now()).unwrap();
        assert_eq!((q.months.clone(), q.year), (vec![12], Some(2025)));
        assert_eq!(q.words, ["tax"]);
        assert_eq!(
            q.extensions.iter().map(String::as_str).collect::<Vec<_>>(),
            DOCUMENTS,
            "\"documents\" in a sentence narrows the type"
        );

        // Singular type words, two-word ones, and receipts (a type that's
        // still searched for).
        let q = parse_at("spreadsheet from october", now()).unwrap();
        assert_eq!((q.words.len(), q.extensions.len()), (0, SPREADSHEETS.len()));
        assert!(q.extensions.contains(&"xlsx".to_string()) && q.extensions.contains(&"csv".into()));
        let q = parse_at("slide deck about the roadmap", now()).unwrap();
        assert_eq!(q.words, ["roadmap"]);
        assert!(q.extensions.contains(&"pptx".to_string()));
        for text in [
            "roadmap slide deck?",
            "quarterly slide show.",
            "the hiring slide deck, please",
        ] {
            let q = parse_at(text, now()).unwrap();
            assert!(
                !q.words.contains(&"slide".to_string()),
                "{text}: {:?}",
                q.words
            );
            assert!(q.extensions.contains(&"pptx".to_string()), "{text}");
        }
        // An explicit ext: filter wins over "receipts", in either order.
        for text in ["receipts ext:txt", "ext:txt receipts"] {
            let q = parse_at(text, now()).unwrap();
            assert_eq!(q.extensions, ["txt"], "{text}");
            assert_eq!(q.words, ["receipts"], "{text}");
        }
        let q = parse_at("budget slideshow", now()).unwrap();
        assert!(q.extensions.contains(&"odp".to_string()));
        let q = parse_at("all my receipts", now()).unwrap();
        assert_eq!(q.words, ["receipts"], "still searched for");
        assert!(q.extensions.contains(&"pdf".to_string()) && q.extensions.contains(&"jpg".into()));
        assert!(!q.extensions.contains(&"docx".to_string()));
        let q = parse_at("a photo of the dog", now()).unwrap();
        assert_eq!(
            (q.words.clone(), q.kind),
            (vec!["dog".to_string()], Some(Kind::Image))
        );

        // A year after "in" is a date; elsewhere it may be part of a name.
        assert_eq!(
            parse_at("receipts in 2024", now()).unwrap().year,
            Some(2024)
        );
        let q = parse_at("report 2025", now()).unwrap();
        assert_eq!(
            (q.words.clone(), q.year),
            (vec!["report".to_string(), "2025".to_string()], None)
        );

        // "photo" on its own lists photos; quoted, it's a name.
        let q = parse_at("photo", now()).unwrap();
        assert!(q.words.is_empty() && q.kind == Some(Kind::Image));
        let q = parse_at("video of a cat", now()).unwrap();
        assert_eq!(
            (q.words.clone(), q.kind),
            (vec!["cat".to_string()], Some(Kind::Video))
        );
        assert_eq!(parse_at("\"photos\"", now()).unwrap().words, ["photos"]);
        // Document types filter even alone (desktop.ini isn't a spreadsheet).
        let q = parse_at("spreadsheets", now()).unwrap();
        assert!(q.words.is_empty() && q.extensions.contains(&"xlsx".to_string()));
        assert_eq!(
            parse_at("\"may\" notes", now()).unwrap().words,
            ["may", "notes"]
        );
        assert_eq!(parse_at("\"show all\"", now()).unwrap().words, ["show all"]);
        let q = parse_at("yesterday", now()).unwrap();
        assert!(q.words.is_empty() && q.between.is_some());
        // Lowercase "in"/"or" are filler or words, never Indiana/Oregon.
        assert!(parse_at("receipts or invoices", now())
            .unwrap()
            .alternatives
            .iter()
            .all(|a| a.is_empty()));
        assert_eq!(
            parse_at("AZ receipts", now()).unwrap().alternatives[0],
            ["arizona"]
        );
    }

    /// Midnight at the start of a day, local time, in ns.
    fn day(y: i32, m: u32, d: u32) -> i64 {
        Local
            .with_ymd_and_hms(y, m, d, 0, 0, 0)
            .unwrap()
            .timestamp_nanos_opt()
            .unwrap()
    }

    #[test]
    fn seasons_holidays_and_ago() {
        // now() is Thursday 2026-10-15.
        let q = parse_at("beach photos last summer", now()).unwrap();
        assert_eq!(q.between, Some((day(2026, 6, 1), day(2026, 9, 1))));
        assert_eq!(q.kind, Some(Kind::Image));
        assert_eq!(q.words, ["beach"]);
        // Summer of any year, and of a named year.
        assert_eq!(parse_at("summer hikes", now()).unwrap().months, [6, 7, 8]);
        assert_eq!(
            parse_at("summer 2024 hikes", now()).unwrap().between,
            Some((day(2024, 6, 1), day(2024, 9, 1)))
        );
        // This winter hasn't started (it's October): last winter began Dec 2025.
        assert_eq!(
            parse_at("last winter", now()).unwrap().between,
            Some((day(2025, 12, 1), day(2026, 3, 1)))
        );

        let q = parse_at("photos from Christmas", now()).unwrap();
        assert_eq!(q.days, Some((1220, 1231)));
        assert_eq!(
            parse_at("Christmas 2024 photos", now()).unwrap().between,
            Some((day(2024, 12, 20), day(2025, 1, 1)))
        );
        assert_eq!(
            parse_at("last Christmas", now()).unwrap().between,
            Some((day(2025, 12, 20), day(2026, 1, 1)))
        );
        let q = parse_at("new year's eve party", now()).unwrap();
        assert_eq!(q.days, Some((1228, 103)));
        assert_eq!(q.words, ["eve", "party"]);

        assert_eq!(
            parse_at("3 weeks ago", now()).unwrap().between,
            Some((day(2026, 9, 21), day(2026, 9, 28)))
        );
        assert_eq!(
            parse_at("notes from a month ago", now()).unwrap().between,
            Some((day(2026, 9, 1), day(2026, 10, 1)))
        );
        assert_eq!(
            parse_at("2 days ago", now()).unwrap().between,
            Some((day(2026, 10, 13), day(2026, 10, 14)))
        );
        assert_eq!(
            parse_at("past 2 weeks", now()).unwrap().between,
            Some((day(2026, 10, 1), day(2026, 10, 16)))
        );
    }

    #[test]
    fn amounts_are_noted_not_searched() {
        let q = parse_at("invoices from last year over 100 dollars", now()).unwrap();
        assert_eq!(q.words, ["invoices"]);
        assert_eq!(q.between, Some((day(2025, 1, 1), day(2026, 1, 1))));
        assert_eq!(q.not_searchable, ["an amount"]);
        assert!(q
            .describe_filters()
            .iter()
            .any(|f| f == "an amount can't be searched yet"));
        let q = parse_at("receipts more than $50", now()).unwrap();
        assert_eq!(q.words, ["receipts"]);
    }

    #[test]
    fn document_kinds_in_sentences() {
        let q = parse_at("slides for the quarterly review", now()).unwrap();
        assert_eq!(q.words, ["quarterly", "review"]);
        assert!(q.extensions.contains(&"pptx".to_string()));
        assert!(parse_at("budget spreadsheets", now())
            .unwrap()
            .extensions
            .contains(&"xlsx".to_string()));
        assert_eq!(
            parse_at("contract pdfs", now()).unwrap().extensions,
            ["pdf"]
        );
        // On their own they're filters too; quotes look for the name.
        assert!(parse_at("slides", now()).unwrap().words.is_empty());
        assert_eq!(parse_at("\"slides\"", now()).unwrap().words, ["slides"]);
        // Question words are filler.
        assert_eq!(
            parse_at("where is the letter from the IRS?", now())
                .unwrap()
                .words,
            ["letter", "irs"]
        );
    }

    #[test]
    fn date_spans_filter_by_day_of_year_even_across_new_year() {
        let (_d, root, conn) = indexed(&["dec-party.txt", "jan-party.txt", "jun-party.txt"]);
        let set = |name: &str, y: i32, m: u32, d: u32| {
            let t = Local.with_ymd_and_hms(y, m, d, 12, 0, 0).unwrap();
            conn.execute(
                "UPDATE files SET modified_ns = ?1 WHERE filename = ?2",
                rusqlite::params![t.timestamp_nanos_opt().unwrap(), name],
            )
            .unwrap();
        };
        set("dec-party.txt", 2024, 12, 30);
        set("jan-party.txt", 2025, 1, 2);
        set("jun-party.txt", 2025, 6, 15);
        let mut found = search(
            &conn,
            root.path(),
            "new year party",
            SearchScope::Everywhere,
            "",
        );
        found.sort();
        assert_eq!(found, ["dec-party.txt", "jan-party.txt"]);
        assert_eq!(
            search(
                &conn,
                root.path(),
                "summer party",
                SearchScope::Everywhere,
                ""
            ),
            ["jun-party.txt"]
        );
    }

    #[test]
    fn full_text_queries_are_quoted_so_nothing_typed_is_syntax() {
        let q = parse_at("NEAR(a b) \"x\"\"y\" -c *", now()).unwrap();
        let m = fts_match(&q, true);
        // Every term is a quoted string; FTS operators are just words.
        assert!(
            m.split(" AND ")
                .flat_map(|t| t.split(" OR "))
                .all(|t| t.trim_matches(|c| c == '(' || c == ')').starts_with('"')),
            "{m}"
        );
    }

    #[test]
    fn fusion_keeps_names_first_and_adds_meaning_matches() {
        let c = |n: &str, reason: MatchReason| Candidate {
            native: n.as_bytes().to_vec(),
            reason,
            words: 1.0,
        };
        let ranked = vec![
            c("name-match", MatchReason::NameContains),
            c("text-a", MatchReason::Content("«x»".into())),
            c("text-b", MatchReason::Folder),
        ];
        let similar = vec![
            (b"text-b".to_vec(), 0.8, MatchReason::Meaning(0.8)),
            (b"meaning-only".to_vec(), 0.7, MatchReason::Meaning(0.7)),
        ];
        let fused: Vec<String> = fuse(ranked, &similar)
            .into_iter()
            .map(|c| String::from_utf8(c.native).unwrap())
            .collect();
        // The name match stays first; text-b, close in meaning, passes
        // text-a; a close meaning-only match passes a word-only match.
        assert_eq!(fused, ["name-match", "text-b", "meaning-only", "text-a"]);
    }

    #[test]
    fn relative_dates_start_at_midnight() {
        let q = parse_at("modified:week", now()).unwrap();
        let (cmp, ns) = q.modified.unwrap();
        assert_eq!(cmp, Cmp::Greater);
        // 2026-10-15 is a Thursday; the week began Monday the 12th.
        let monday = Local.with_ymd_and_hms(2026, 10, 12, 0, 0, 0).unwrap();
        assert_eq!(ns, monday.timestamp_nanos_opt().unwrap());
    }

    struct Go;
    impl ScanControl for Go {
        fn keep_going(&self) -> bool {
            true
        }
        fn progress(&self, _files: u64) {}
        fn database_full(&self) -> bool {
            false
        }
    }

    fn indexed(files: &[&str]) -> (tempfile::TempDir, tempfile::TempDir, Connection) {
        let db_dir = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        for f in files {
            let p = root.path().join(f);
            if f.ends_with('/') {
                fs::create_dir_all(&p).unwrap();
            } else {
                fs::create_dir_all(p.parent().unwrap()).unwrap();
                fs::write(&p, vec![0u8; f.len()]).unwrap();
            }
        }
        let key = SecretBytes::random(KEY_LEN).unwrap();
        let mut conn = open_test_db(db_dir.path(), &key);
        let exclusions = Exclusions::with_system_paths(&IndexSettings::default(), None, Vec::new());
        scan_root(&mut conn, root.path(), &exclusions, &Go, Duration::ZERO).unwrap();
        (db_dir, root, conn)
    }

    fn search(
        conn: &Connection,
        root: &Path,
        text: &str,
        scope: SearchScope,
        folder: &str,
    ) -> Vec<String> {
        let request = SearchRequest {
            generation: 1,
            text: text.into(),
            scope,
            folder: root.join(folder),
            offset: 0,
            limit: PAGE_SIZE,
            full_text: true,
            smart: false,
            understand: false,
        };
        let (results, _) = run(conn, &request, &parse(text).unwrap(), &[], &[], &|| false).unwrap();
        results.hits.iter().map(|h| h.item.name.clone()).collect()
    }

    #[test]
    fn pictures_showing_the_words_are_found_through_the_same_filters() {
        let (_d, root, conn) =
            indexed(&["IMG_0001.jpg", "IMG_0002.jpg", "clip.mp4", "mountains.txt"]);
        let id = |name: &str| -> i64 {
            conn.query_row("SELECT id FROM files WHERE filename = ?1", [name], |r| {
                r.get(0)
            })
            .unwrap()
        };
        // What the picture model found for "mountains": two photos and a video.
        let pictures = vec![
            (id("IMG_0002.jpg"), 0.07),
            (id("clip.mp4"), 0.05),
            (id("IMG_0001.jpg"), 0.03),
        ];
        let ask = |text: &str| -> Vec<(String, MatchReason)> {
            let request = SearchRequest {
                generation: 1,
                text: text.into(),
                scope: SearchScope::Everywhere,
                folder: root.path().to_path_buf(),
                offset: 0,
                limit: PAGE_SIZE,
                full_text: true,
                smart: true,
                understand: false,
            };
            let (results, _) = run(
                &conn,
                &request,
                &parse(text).unwrap(),
                &[],
                &pictures,
                &|| false,
            )
            .unwrap();
            results
                .hits
                .into_iter()
                .map(|h| (h.item.name, h.reason))
                .collect()
        };
        let found = ask("mountains");
        let names: Vec<&str> = found.iter().map(|(n, _)| n.as_str()).collect();
        // The name match first, then pictures by how clearly they show it.
        assert_eq!(
            names,
            ["mountains.txt", "IMG_0002.jpg", "clip.mp4", "IMG_0001.jpg"]
        );
        assert_eq!(found[1].1, MatchReason::Picture);
        // "photo of" keeps only photos; "video of" only videos.
        let names = |text: &str| ask(text).into_iter().map(|(n, _)| n).collect::<Vec<_>>();
        assert_eq!(
            names("photo of mountains"),
            ["IMG_0002.jpg", "IMG_0001.jpg"]
        );
        assert_eq!(names("video of mountains"), ["clip.mp4"]);
    }

    #[test]
    fn a_type_search_never_shows_another_type_whatever_found_it() {
        let (_d, root, conn) = indexed(&["IMG_0001.jpg", "clip.mp4", "process.rs", "notes.txt"]);
        let id = |name: &str| -> i64 {
            conn.query_row("SELECT id FROM files WHERE filename = ?1", [name], |r| {
                r.get(0)
            })
            .unwrap()
        };
        // Meaning and picture matches that include files of other types.
        let hash = |name: &str| -> Vec<u8> { name.as_bytes().to_vec() };
        let meaning = vec![
            (id("process.rs"), hash("process.rs"), 0.53),
            (id("notes.txt"), hash("notes.txt"), 0.52),
        ];
        let pictures = vec![
            (id("process.rs"), 0.09),
            (id("clip.mp4"), 0.05),
            (id("IMG_0001.jpg"), 0.04),
        ];
        let ask = |text: &str| -> Vec<String> {
            let request = SearchRequest {
                generation: 1,
                text: text.into(),
                scope: SearchScope::Everywhere,
                folder: root.path().to_path_buf(),
                offset: 0,
                limit: PAGE_SIZE,
                full_text: true,
                smart: true,
                understand: false,
            };
            let (results, _) = run(
                &conn,
                &request,
                &parse(text).unwrap(),
                &meaning,
                &pictures,
                &|| false,
            )
            .unwrap();
            results.hits.into_iter().map(|h| h.item.name).collect()
        };
        assert_eq!(ask("video of a cat"), ["clip.mp4"]);
        assert_eq!(ask("photo of a cat"), ["IMG_0001.jpg"]);
        assert_eq!(ask("photos"), ["IMG_0001.jpg"]);
        assert_eq!(ask("videos"), ["clip.mp4"]);
        // The final check itself, for any path that slipped past the filters.
        let q = parse("video of a cat").unwrap();
        assert!(!of_wanted_type(
            &q,
            std::path::Path::new("/x/process.rs"),
            false
        ));
        assert!(of_wanted_type(
            &q,
            std::path::Path::new("/x/Cat.MP4"),
            false
        ));
        assert!(
            !of_wanted_type(&q, std::path::Path::new("/x/cats.mp4"), true),
            "a folder isn't a video"
        );
        assert!(of_wanted_type(
            &parse("cat").unwrap(),
            std::path::Path::new("/x/process.rs"),
            false
        ));
    }

    #[test]
    fn a_receipt_read_by_ocr_is_found_by_its_words_and_code_never_is() {
        let (_d, root, mut conn) = indexed(&[
            "Downloads/IMG_2041.jpg",
            "Downloads/statement.pdf",
            "src/hotel.rs",
            "tools/rails.py",
            "notes/conference-hall.md",
        ]);
        let id = |conn: &Connection, name: &str| -> i64 {
            conn.query_row("SELECT id FROM files WHERE filename = ?1", [name], |r| {
                r.get(0)
            })
            .unwrap()
        };
        // The photo's text, as OCR read it.
        let ocr = "GRAND CONFERENCE HALL HOTEL\nRoom 12 $180.00\nTOTAL $180.00";
        let receipt = id(&conn, "IMG_2041.jpg");
        super::super::db::store_ocr_text(
            &mut conn,
            receipt,
            ocr,
            &super::super::extractor::chunks(ocr),
        )
        .unwrap();
        // Source files saying the same words.
        for name in ["hotel.rs", "rails.py", "conference-hall.md"] {
            let text = "// hotel conference hall receipt rails";
            let chunks = super::super::extractor::chunks(text);
            let contents = super::super::db::Contents {
                state: 1,
                error: None,
                text,
                chunks: &chunks,
                taken_ns: None,
                gps: None,
                place: None,
            };
            let file = id(&conn, name);
            super::super::db::store_contents(&mut conn, file, &contents).unwrap();
        }
        let ask = |text: &str| search(&conn, root.path(), text, SearchScope::Everywhere, "");
        // Found by words printed on it.
        assert!(ask("conference hall").contains(&"IMG_2041.jpg".to_string()));
        assert!(ask("Hotel").contains(&"IMG_2041.jpg".to_string()));
        // A receipt search shows only what a receipt can be.
        for text in [
            "hotel receipt",
            "conference hall receipts",
            "hotel reciept",
            "hotel invoice",
            "scanned hotel",
        ] {
            let found = ask(text);
            assert!(
                found.contains(&"IMG_2041.jpg".to_string()),
                "{text}: {found:?}"
            );
            for code in ["hotel.rs", "rails.py", "conference-hall.md"] {
                assert!(!found.contains(&code.to_string()), "{text}: {found:?}");
            }
        }
        // Code that mentions rails is not a receipt about rails.
        assert!(ask("rails reciept").is_empty());
        assert_eq!(
            ask("rails").len(),
            3,
            "without a receipt word, code is found"
        );
    }

    #[test]
    fn every_typed_word_must_be_in_the_text() {
        let (_d, root, mut conn) = indexed(&["receipt.pdf", "fake.rs", "street.md", "llc.txt"]);
        let texts = [
            (
                "receipt.pdf",
                "Receipt Example, LLC 123 Fake Street New York",
            ),
            ("fake.rs", "// a fake clipboard for tests"),
            ("street.md", "Street names and example addresses"),
            ("llc.txt", "Some LLC paperwork, for example"),
        ];
        for (name, text) in texts {
            let id: i64 = conn
                .query_row("SELECT id FROM files WHERE filename = ?1", [name], |r| {
                    r.get(0)
                })
                .unwrap();
            let chunks = super::super::extractor::chunks(text);
            let contents = super::super::db::Contents {
                state: 1,
                error: None,
                text,
                chunks: &chunks,
                taken_ns: None,
                gps: None,
                place: None,
            };
            super::super::db::store_contents(&mut conn, id, &contents).unwrap();
        }
        let ask = |text: &str| search(&conn, root.path(), text, SearchScope::Everywhere, "");
        assert_eq!(ask("Fake Street"), ["receipt.pdf"]);
        assert_eq!(ask("Example LLC"), ["receipt.pdf"]);
        // Both words, but apart: only when no file has the phrase.
        assert_eq!(ask("example names"), ["street.md"]);
        // Not the phrase in that order: every file with both words.
        let mut both = ask("LLC example");
        both.sort();
        assert_eq!(both, ["llc.txt", "receipt.pdf"]);
        // A type word narrows the type; it needn't be in the text.
        assert_eq!(ask("Fake Street receipt"), ["receipt.pdf"]);
        assert_eq!(ask("fake"), ["fake.rs", "receipt.pdf"]);
    }

    #[test]
    fn misspelled_receipts_and_invoices_narrow_to_pdfs_and_pictures() {
        for text in ["rails reciept", "reciepts", "invoce from Sedona", "scans"] {
            let q = parse_at(text, now()).unwrap();
            assert!(q.extensions.contains(&"pdf".to_string()), "{text}");
            assert!(q.extensions.contains(&"heic".to_string()), "{text}");
            assert!(!q.extensions.contains(&"md".to_string()), "{text}");
        }
        assert_eq!(
            parse_at("rails reciept", now()).unwrap().words,
            ["rails", "receipt"]
        );
        // An explicit extension still wins.
        assert_eq!(
            parse_at("receipt ext:txt", now()).unwrap().extensions,
            ["txt"]
        );
    }

    #[test]
    fn recently_saved_images_are_offered_to_ocr_first() {
        let (_d, _root, conn) = indexed(&["old/scan.png", "Downloads/receipt.jpg"]);
        conn.execute("UPDATE files SET extraction_state = 1", [])
            .unwrap();
        let now_ns = super::super::db::unix_now() * 1_000_000_000;
        let day = 24 * 3600 * 1_000_000_000i64;
        conn.execute(
            "UPDATE files SET modified_ns = ?1 WHERE filename = 'scan.png'",
            [now_ns - 400 * day],
        )
        .unwrap();
        conn.execute(
            "UPDATE files SET modified_ns = ?1 WHERE filename = 'receipt.jpg'",
            [now_ns],
        )
        .unwrap();
        let since = now_ns - 7 * day;
        let recent = super::super::db::ocr_candidates(&conn, 10, since).unwrap();
        let names: Vec<_> = recent
            .iter()
            .map(|p| p.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["receipt.jpg"]);
        assert_eq!(
            super::super::db::ocr_candidate_count(&conn, since).unwrap(),
            1
        );
        assert_eq!(
            super::super::db::ocr_candidate_count(&conn, i64::MIN).unwrap(),
            2
        );
    }

    #[test]
    fn emphasis_reorders_but_never_filters() {
        let (_d, root, conn) = indexed(&["notes.txt", "draft.txt", "lunch.txt"]);
        for (name, text) in [
            ("notes.txt", "call with acme"),
            ("draft.txt", "contract terms"),
            ("lunch.txt", "soup"),
        ] {
            conn.execute(
                "INSERT INTO file_fts (rowid, filename, display_path, place, content)
                 SELECT id, filename_folded, '', '', ?2 FROM files WHERE filename_folded = ?1",
                [name, text],
            )
            .unwrap();
        }
        let request = SearchRequest {
            generation: 1,
            text: String::new(),
            scope: SearchScope::Everywhere,
            folder: root.path().to_path_buf(),
            offset: 0,
            limit: PAGE_SIZE,
            full_text: true,
            smart: false,
            understand: true,
        };
        let names = |emphasis: &[&str]| -> Vec<String> {
            let mut q = parse("the contract with acme").unwrap();
            q.emphasis = emphasis.iter().map(|w| w.to_string()).collect();
            let (results, _) = run(&conn, &request, &q, &[], &[], &|| false).unwrap();
            results.hits.iter().map(|h| h.item.name.clone()).collect()
        };
        assert_eq!(names(&["acme"]), ["notes.txt", "draft.txt"]);
        assert_eq!(names(&["contract"]), ["draft.txt", "notes.txt"]);
    }

    #[test]
    fn exact_and_prefix_names_rank_first_and_order_is_stable() {
        let (_d, root, conn) = indexed(&[
            "old/my-report-draft.txt",
            "report.pdf",
            "reports/",
            "a/annual report summary.txt",
            "report-2025.xlsx",
        ]);
        let found = search(&conn, root.path(), "report", SearchScope::Everywhere, "");
        assert_eq!(found[0], "report.pdf");
        assert_eq!(found[1], "reports");
        assert_eq!(found[2], "report-2025.xlsx");
        assert_eq!(found.len(), 5);
        // Deterministic: the same search, the same order.
        assert_eq!(
            found,
            search(&conn, root.path(), "report", SearchScope::Everywhere, "")
        );
    }

    #[test]
    fn subfolder_scope_and_filters_narrow_results() {
        let (_d, root, conn) = indexed(&[
            "Photos/beach.jpg",
            "Photos/2025/party.png",
            "Photos/notes.txt",
            "Docs/beach-trip.pdf",
        ]);
        assert_eq!(
            search(
                &conn,
                root.path(),
                "beach",
                SearchScope::Subfolders,
                "Photos"
            ),
            ["beach.jpg"]
        );
        let mut images = search(
            &conn,
            root.path(),
            "type:image",
            SearchScope::Everywhere,
            "",
        );
        images.sort();
        assert_eq!(images, ["beach.jpg", "party.png"]);
        assert_eq!(
            search(&conn, root.path(), "ext:pdf", SearchScope::Everywhere, ""),
            ["beach-trip.pdf"]
        );
        assert_eq!(
            search(
                &conn,
                root.path(),
                "type:folder 2025",
                SearchScope::Everywhere,
                ""
            ),
            ["2025"]
        );
        // Files are as long as their names here; this one is 18 bytes.
        assert_eq!(
            search(
                &conn,
                root.path(),
                "size:>17 beach",
                SearchScope::Everywhere,
                ""
            ),
            ["beach-trip.pdf"]
        );
    }

    #[test]
    fn missing_or_unreadable_files_are_not_shown_and_are_reported_stale() {
        let (_d, root, conn) = indexed(&["keep.txt", "gone.txt"]);
        fs::remove_file(root.path().join("gone.txt")).unwrap();
        let request = SearchRequest {
            generation: 1,
            text: "txt".into(),
            scope: SearchScope::Everywhere,
            folder: root.path().into(),
            offset: 0,
            limit: PAGE_SIZE,
            full_text: true,
            smart: false,
            understand: false,
        };
        let (results, stale) =
            run(&conn, &request, &parse("txt").unwrap(), &[], &[], &|| false).unwrap();
        assert_eq!(results.hits.len(), 1);
        assert_eq!(stale, [root.path().join("gone.txt")]);
    }

    #[test]
    fn results_come_in_pages() {
        let names: Vec<String> = (0..25).map(|i| format!("file{i:02}.txt")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let (_d, root, conn) = indexed(&refs);
        let page = |offset| {
            let request = SearchRequest {
                generation: 1,
                text: "file".into(),
                scope: SearchScope::Everywhere,
                folder: root.path().into(),
                offset,
                limit: 10,
                full_text: true,
                smart: false,
                understand: false,
            };
            run(&conn, &request, &parse("file").unwrap(), &[], &[], &|| {
                false
            })
            .unwrap()
            .0
        };
        let first = page(0);
        assert_eq!(first.hits.len(), 10);
        assert!(first.more);
        let last = page(20);
        assert_eq!(last.hits.len(), 5);
        assert!(!last.more);
    }

    #[test]
    fn a_cancelled_search_stops() {
        let (_d, root, conn) = indexed(&["a1.txt", "a2.txt", "a3.txt"]);
        let request = SearchRequest {
            generation: 1,
            text: "a".into(),
            scope: SearchScope::Everywhere,
            folder: root.path().into(),
            offset: 0,
            limit: PAGE_SIZE,
            full_text: true,
            smart: false,
            understand: false,
        };
        let (results, _) = run(&conn, &request, &parse("a").unwrap(), &[], &[], &|| true).unwrap();
        assert!(results.hits.is_empty());
    }
}
