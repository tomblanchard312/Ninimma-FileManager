// Ninimma modification: independent project branding and integration.
//! Experimental: a small local model picks the important words of a search
//! written as a sentence.
//!
//! The fixed rules in `index::search` stay in charge. Measured on test
//! sentences, small models (0.5B-1.5B) got dates wrong, invented filters,
//! and sometimes dropped the most important word ("contract" from "the
//! contract I signed with Acme"). So the model can't change what is found,
//! only the order: files matching the words it singles out (which must be
//! words the rules also kept from the search) rank higher. Its output is held to a grammar (well-formed JSON of
//! one shape, nothing else). Anything unexpected is ignored, and the
//! rules' reading is used as is.

// Builds without smart search keep this code (and its tests) but don't
// call all of it.
#![cfg_attr(not(feature = "smart-search"), allow(dead_code))]

#[cfg(feature = "smart-search")]
pub mod local;

use crate::index::search::{Kind, Query};
use crate::models::{self, QUERY_MODEL};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// What the model may say.
pub const GRAMMAR: &str = r#"
root ::= "{" "\"words\":" words ",\"place\":" strnull ",\"kind\":" kind "}"
words ::= "[" ( str ( "," str ){0,5} )? "]"
kind ::= "null" | "\"image\"" | "\"video\"" | "\"audio\""
strnull ::= "null" | str
str ::= "\"" [a-zA-Z0-9 .'&-]{1,32} "\""
"#;

/// The model's answer, once checked.
#[derive(Debug, Default, PartialEq)]
pub struct Understanding {
    pub words: Vec<String>,
    pub place: Option<String>,
    pub kind: Option<Kind>,
}

/// Whether a search is a sentence worth asking the model about: several
/// words, plain ASCII (the model and grammar are English), no explicit
/// filters or quotes (those mean the user is being precise already).
pub fn is_sentence(text: &str) -> bool {
    let words = text.split_whitespace().count();
    (4..=24).contains(&words) && text.is_ascii() && !text.contains(':') && !text.contains('"')
}

/// Reads the model's JSON (already held to `GRAMMAR`) and keeps only what
/// appears in `asked`.
pub fn check(answer: &str, asked: &str) -> Option<Understanding> {
    let asked = asked.to_lowercase();
    let field = |name: &str| -> Option<&str> {
        let start = answer.find(&format!("\"{name}\":"))? + name.len() + 3;
        let rest = &answer[start..];
        let end = if rest.starts_with('[') {
            rest.find(']')? + 1
        } else {
            rest.find([',', '}'])?
        };
        Some(&rest[..end])
    };
    let strings = |s: &str| -> Vec<String> {
        s.split('"')
            .skip(1)
            .step_by(2)
            .map(|w| w.trim().to_lowercase())
            .filter(|w| !w.is_empty())
            .collect()
    };
    // Whole words only ("art" isn't in "party"); "grandma" matches
    // "grandma's".
    let asked_words: Vec<String> = asked
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .trim_end_matches("'s")
                .to_string()
        })
        .collect();
    let in_question = |w: &String| {
        w.split_whitespace()
            .all(|part| asked_words.iter().any(|a| a == part))
    };
    let mut words: Vec<String> = strings(field("words")?)
        .into_iter()
        .filter(in_question)
        .collect();
    words.dedup();
    let place = strings(field("place")?).into_iter().find(in_question);
    let kind = match field("kind")? {
        "\"image\"" => Some(Kind::Image),
        "\"video\"" => Some(Kind::Video),
        "\"audio\"" => Some(Kind::Audio),
        _ => None,
    };
    (!words.is_empty() || place.is_some()).then_some(Understanding { words, place, kind })
}

/// The rules' reading, with the words the model singled out marked for
/// emphasis. Only words the rules kept count (so no date, filler or new
/// word slips in); nothing is added, removed or filtered.
pub fn merge(mut rules: Query, model: &Understanding) -> Query {
    let base = |w: &str| w.trim_end_matches("'s").to_string();
    let picked: Vec<String> = model
        .words
        .iter()
        .chain(model.place.iter())
        .map(|w| base(w))
        .collect();
    rules.emphasis = rules
        .words
        .iter()
        .filter(|w| picked.contains(&base(w)))
        .cloned()
        .collect();
    rules
}

/// Answers for recent searches, so typing the same sentence again (or
/// paging through results) doesn't run the model twice.
#[derive(Default)]
struct Cache {
    answers: HashMap<String, Option<Understanding>>,
    order: Vec<String>,
}

impl Cache {
    const SIZE: usize = 64;

    fn get(&self, text: &str) -> Option<&Option<Understanding>> {
        self.answers.get(text)
    }

    fn put(&mut self, text: &str, answer: Option<Understanding>) {
        if self.order.len() >= Self::SIZE {
            let oldest = self.order.remove(0);
            self.answers.remove(&oldest);
        }
        self.order.push(text.to_string());
        self.answers.insert(text.to_string(), answer);
    }
}

/// Runs the model on one question.
pub trait Interpreter: Send {
    /// The raw JSON answer, or None if cancelled.
    fn interpret(&mut self, question: &str, cancelled: &dyn Fn() -> bool) -> Option<String>;
}

/// The shared, lazily loaded query model.
pub struct Understander {
    path: Option<std::path::PathBuf>,
    loaded: Mutex<Option<(Box<dyn Interpreter>, Instant)>>,
    cache: Mutex<Cache>,
}

impl Default for Understander {
    fn default() -> Self {
        Self::new()
    }
}

impl Understander {
    pub fn new() -> Self {
        Self {
            path: None,
            loaded: Mutex::new(None),
            cache: Mutex::new(Cache::default()),
        }
    }

    pub fn at(path: std::path::PathBuf) -> Self {
        Self {
            path: Some(path),
            ..Self::new()
        }
    }

    fn model_path(&self) -> Option<std::path::PathBuf> {
        let path = self
            .path
            .clone()
            .or_else(|| models::path_of(&QUERY_MODEL))?;
        std::fs::metadata(&path)
            .ok()
            .filter(|m| m.is_file() && m.len() == QUERY_MODEL.size)
            .map(|_| path)
    }

    pub fn is_available(&self) -> bool {
        crate::embedding::is_built_in() && self.model_path().is_some()
    }

    /// The model's reading of `question`, checked against it; None if it
    /// had nothing useful to add, failed, or was cancelled.
    pub fn understand(
        &self,
        question: &str,
        cancelled: &dyn Fn() -> bool,
    ) -> Option<Understanding> {
        if let Some(known) = self.cache.lock().unwrap().get(question) {
            return known.as_ref().map(|u| Understanding {
                words: u.words.clone(),
                place: u.place.clone(),
                kind: u.kind,
            });
        }
        let answer = {
            let mut loaded = self.loaded.lock().unwrap();
            if loaded.is_none() {
                *loaded = Some((self.load()?, Instant::now()));
            }
            let (model, used) = loaded.as_mut().expect("loaded");
            *used = Instant::now();
            model.interpret(question, cancelled)?
        };
        let checked = check(&answer, question);
        let copy = checked.as_ref().map(|u| Understanding {
            words: u.words.clone(),
            place: u.place.clone(),
            kind: u.kind,
        });
        self.cache.lock().unwrap().put(question, copy);
        checked
    }

    #[cfg(feature = "smart-search")]
    fn load(&self) -> Option<Box<dyn Interpreter>> {
        let path = self.model_path()?;
        models::verify(&QUERY_MODEL, &path).ok()?;
        local::QueryModel::load(&path)
            .ok()
            .map(|m| Box::new(m) as Box<dyn Interpreter>)
    }

    #[cfg(not(feature = "smart-search"))]
    fn load(&self) -> Option<Box<dyn Interpreter>> {
        None
    }

    pub fn unload_if_idle(&self, idle: Duration) {
        let mut loaded = self.loaded.lock().unwrap();
        if loaded
            .as_ref()
            .is_some_and(|(_, used)| used.elapsed() >= idle)
        {
            *loaded = None;
        }
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded.lock().unwrap().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::search::parse;

    #[test]
    fn only_words_from_the_question_are_kept() {
        let asked = "the contract I signed with Acme in March 2025";
        let u = check(
            r#"{"words":["contract","Acme","lawyer"],"place":null,"kind":null}"#,
            asked,
        )
        .unwrap();
        assert_eq!(u.words, ["contract", "acme"], "\"lawyer\" wasn't asked for");
        // Whole words only.
        assert_eq!(
            check(
                r#"{"words":["art","party"],"place":null,"kind":null}"#,
                "birthday party photos"
            )
            .unwrap()
            .words,
            ["party"]
        );
        // A made-up place is dropped too; nothing left means no answer.
        assert_eq!(
            check(r#"{"words":["lawyer"],"place":"Paris","kind":null}"#, asked),
            None
        );
        let u = check(
            r#"{"words":["videos"],"place":"Paris","kind":"video"}"#,
            "videos from our trip to Paris",
        )
        .unwrap();
        assert_eq!(
            (u.place.as_deref(), u.kind),
            (Some("paris"), Some(Kind::Video))
        );
        assert_eq!(check("not json", asked), None);
    }

    #[test]
    fn the_model_only_reorders_never_filters() {
        let asked = "the contract I signed with Acme in March 2025";
        let rules = parse(asked).unwrap();
        // What the real model said: it dropped "contract", kept dates as
        // words, and called it audio.
        let model = Understanding {
            words: vec!["acme".into(), "march".into(), "2025".into()],
            place: Some("acme".into()),
            kind: Some(Kind::Audio),
        };
        let merged = merge(rules.clone(), &model);
        assert_eq!(merged.words, rules.words, "every word is still searched");
        assert_eq!(
            merged.emphasis,
            ["acme"],
            "only words the rules kept are emphasised"
        );
        assert_eq!(
            (merged.kind, merged.months.clone(), merged.year),
            (rules.kind, rules.months, rules.year)
        );
        // "grandma" matches "grandma's".
        let merged = merge(
            parse("pictures from grandma's birthday party last month").unwrap(),
            &Understanding {
                words: vec!["birthday".into()],
                place: Some("grandma".into()),
                kind: None,
            },
        );
        assert_eq!(merged.emphasis, ["grandma's", "birthday"]);
    }

    #[test]
    fn only_plain_sentences_go_to_the_model() {
        assert!(is_sentence("receipts from my Arizona trip"));
        assert!(!is_sentence("tax 2024"), "too short to need it");
        assert!(
            !is_sentence("ext:pdf from my Arizona trip"),
            "explicit filters"
        );
        assert!(!is_sentence("\"exact phrase\" from my trip"));
        assert!(
            !is_sentence("Rechnungen aus München letzten Monat"),
            "the model is English-only"
        );
    }

    #[test]
    fn the_cache_is_bounded() {
        let mut cache = Cache::default();
        for i in 0..(Cache::SIZE + 10) {
            cache.put(&format!("q{i}"), None);
        }
        assert_eq!(cache.answers.len(), Cache::SIZE);
        assert!(cache.get("q0").is_none());
        assert!(cache.get(&format!("q{}", Cache::SIZE + 9)).is_some());
    }
}
