// Ninimma modification: independent project branding and integration.
//! Indexing and search timings for the opt-in benchmark suite (see `crate::bench`):
//! `cargo test --release bench_ -- --ignored --nocapture --test-threads=1`.

use super::*;
use crate::bench::{measure, ms, scratch};

/// Short documents with a few words each, spread over folders.
fn make_files(root: &Path, n: usize) {
    const WORDS: &[&str] = &[
        "invoice",
        "receipt",
        "holiday",
        "budget",
        "meeting",
        "lease",
        "recipe",
        "report",
        "arizona",
        "garden",
        "insurance",
        "travel",
        "school",
        "taxes",
        "project",
        "notes",
    ];
    for i in 0..n {
        let dir = root.join(format!("folder {}", i % 50));
        if i < 50 {
            std::fs::create_dir(&dir).unwrap();
        }
        let a = WORDS[i % WORDS.len()];
        let b = WORDS[(i / WORDS.len()) % WORDS.len()];
        std::fs::write(
            dir.join(format!("{a} {b} {i}.txt")),
            format!(
                "Document {i} about {a} and {b}. Reference number {}.",
                i * 7919
            ),
        )
        .unwrap();
    }
}

#[test]
#[ignore]
fn bench_index_and_search() {
    const FILES: usize = 10_000;
    let root = scratch();
    make_files(root.path(), FILES);
    let start = Instant::now();
    let mut r = start_with(
        Arc::new(MemoryKeyStore::default()),
        tempfile::tempdir().unwrap(),
        root,
    );
    r.wait_longer("the scan", Duration::from_secs(600), |e| {
        matches!(e, Event::Status(s) if s.state == IndexState::Ready && s.indexed_files >= FILES as u64)
    });
    let took = start.elapsed();
    eprintln!(
        "BENCH index {FILES} text files (names and contents)  {:>10.0} ms  ({:.0} files/s)",
        ms(took),
        FILES as f64 / took.as_secs_f64()
    );
    for query in [
        "receipt",
        "arizona travel",
        "\"reference number 7919\"",
        "type:document",
    ] {
        measure(&format!("search {query}"), 9, || {
            let generation = r.service.search(SearchRequest {
                generation: 0,
                text: query.into(),
                scope: SearchScope::Everywhere,
                folder: r.root.path().into(),
                offset: 0,
                limit: 50,
                full_text: true,
                smart: false,
                understand: false,
            });
            // Polled finely so the timing isn't rounded to the tests' 10 ms.
            loop {
                if r.service.poll().iter().any(|e| match e {
                    Event::Results(x) => x.generation == generation,
                    Event::SearchError {
                        generation: g,
                        message,
                    } if *g == generation => {
                        panic!("{query}: {message}")
                    }
                    _ => false,
                }) {
                    break;
                }
                std::thread::sleep(Duration::from_micros(200));
            }
        });
    }
}
