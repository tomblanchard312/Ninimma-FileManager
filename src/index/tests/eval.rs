// Ninimma modification: independent project branding and integration.
//! How well smart search finds documents described in words, measured on
//! a small, realistic set: receipts among other papers, and "Arizona trip"
//! files that are not receipts, to catch ranking by words alone. Needs the
//! real model, so it's opt-in (the files are made from scratch by
//! `fixtures`):
//! `NINIMMA_TEST_MODEL=<bge .gguf> cargo test --release --features
//! smart-search -- --ignored --nocapture finds_documents_by_meaning`.

use super::*;
use std::time::SystemTime;

use super::fixtures;

enum Make {
    Text,
    /// A PDF with real text; `true` for receipt-shaped paper.
    Pdf(bool),
    /// A scanned page: an image-only PDF, readable only with OCR.
    Scan,
    Word,
    /// A phone photo of a receipt: taken, latitude, longitude.
    ReceiptPhoto(&'static str, f64, f64),
    /// An ordinary photo (red rocks): taken, latitude, longitude.
    Photo(&'static str, f64, f64),
}
use Make::*;

const SEDONA: (f64, f64) = (34.8697, -111.7610);
const FLAGSTAFF: (f64, f64) = (35.1983, -111.6513);

/// (path, how it's made, text, month in 2025 it was last changed)
const DOCS: &[(&str, Make, &str, u32)] = &[
    // Receipts from the Arizona trip, in December.
    ("Trips/scan_0012.pdf", Scan, "RED ROCK LODGE\n150 Canyon Rd, Sedona, AZ 86336\nGuest folio\nCheck-in 12/18/2025\nCheck-out 12/21/2025\nRoom 3 nights x $137.50\nTOTAL $412.50\nVISA ****4417 PAID\nThank you for staying with us", 12),
    ("Pictures/IMG_20251219_0931.jpg", ReceiptPhoto("2025:12:19 09:31:00", FLAGSTAFF.0, FLAGSTAFF.1), "Chevron #20931\nFlagstaff AZ 86001\nPUMP 07 UNLEADED\n14.212 GAL @ $3.459\nFUEL TOTAL $49.16\nDEBIT APPROVED\nAUTH 044191", 12),
    ("Documents/Elote Cafe.pdf", Pdf(true), "ELOTE CAFE\nSedona, Arizona\nTable 12 Server Maria\n2 Enchiladas 38.00\n1 Elote 12.00\n2 Margaritas 36.00\nSubtotal 86.00\nTax 8.17\nTip 17.20\nTotal 111.37\nThank you!", 12),
    ("Downloads/booking-confirmation.pdf", Pdf(false), "Pink Jeep Tours - Booking confirmation\nBroken Arrow tour, Sedona AZ, December 20, 2025, 2 adults\nAmount charged: $198.00 to VISA ending 4417\nThis is your receipt.", 12),
    // Receipts, but not from Arizona.
    ("Documents/costco.pdf", Pdf(true), "COSTCO WHOLESALE #1031\nSeattle WA\nPaper towels 19.99\nEggs 2 dz 6.49\nRotisserie chicken 4.99\nTOTAL 31.47\nVISA APPROVED", 11),
    ("Documents/hardware-store.pdf", Pdf(true), "Ace Hardware, Portland OR\nDrill bit set 24.99\nWood screws 7.49\nTotal 32.48, card ending 4417\nReturns within 30 days with receipt.", 6),
    // About the Arizona trip, but not receipts.
    ("Documents/Arizona trip plan.docx", Word, "Arizona trip plan for December\nDay 1: fly to Phoenix, drive to Sedona.\nDay 2: hike Cathedral Rock.\nDay 3: Grand Canyon south rim.\nPack layers, it gets cold at night.", 11),
    ("Documents/arizona-trip-packing.txt", Text, "Packing list for the Arizona trip: hiking boots, fleece, sunscreen, water bottles, camera, chargers, rain jacket.", 11),
    ("Pictures/IMG_20251220_1102.jpg", Photo("2025:12:20 11:02:00", SEDONA.0, SEDONA.1), "", 12),
    // Other papers.
    ("Documents/lease.pdf", Pdf(false), "RESIDENTIAL LEASE AGREEMENT\nThis agreement is made between Landlord Pine Street Properties LLC and Tenant. The premises at 44 Pine St Apt 3 are leased for twelve months. Monthly rent of $1,850 is due on the first of each month. Security deposit $1,850.", 3),
    ("Documents/1040-2024.pdf", Pdf(false), "Form 1040 U.S. Individual Income Tax Return 2024\nFiling status: single. Wages, salaries, tips 84,210. Taxable interest 312. Adjusted gross income 84,522. Standard deduction 14,600. Refund 1,204.", 4),
    ("Documents/acme-invoice-2231.pdf", Pdf(false), "INVOICE #2231\nAcme Web Design LLC\nBill to: Tom\nWebsite redesign, 24 hours at $95.00\nAmount due $2,280.00 by January 15. Please pay by bank transfer.", 12),
    ("Documents/team meeting notes.docx", Word, "Team meeting notes\nQ3 roadmap review, hiring two engineers, the travel budget for the offsite was approved.\nAction items: Sam drafts the plan.", 9),
    ("Documents/bank-statement.pdf", Pdf(false), "First National Bank - Statement\nChecking account ending 0021\nOpening balance 4,210.55. Deposits 6,100.00. Withdrawals and debits 3,877.12. Closing balance 6,433.43.", 12),
    ("Documents/sourdough.txt", Text, "Sourdough bread recipe: 500 g flour, 350 g water, 100 g starter, 10 g salt. Bulk ferment 5 hours, shape, proof overnight in the fridge, bake at 250 C.", 2),
    // Work files from October, and one spreadsheet among them.
    ("Documents/household-budget.csv", Text, "Month,Rent,Groceries,Utilities,Fuel,Total\nAugust,1850,612.40,188.20,140.00,2790.60\nSeptember,1850,587.15,176.90,152.30,2766.35\nOctober,1850,640.75,201.10,138.60,2830.45", 10),
    ("Documents/main.json", Text, "{\"name\": \"azurite-config\", \"version\": 3, \"blobPort\": 10000, \"queuePort\": 10001, \"tablePort\": 10002, \"loose\": true}", 10),
    ("Documents/test.log", Text, "2025-10-06 15:18:02 INFO starting test run\n2025-10-06 15:18:04 WARN retrying connection to 127.0.0.1:10000\n2025-10-06 15:18:09 INFO 42 passed, 0 failed", 10),
    ("Documents/REQUIREMENTS.md", Text, "# Requirements\n\nThe file manager must show folders in a tree, support tabs, and search inside documents. Searches must finish in under a second for 100,000 files.", 10),
    ("Documents/generate.py", Text, "import csv\n\ndef generate(rows, out):\n    writer = csv.writer(out)\n    for row in rows:\n        writer.writerow(row)\n", 10),
    ("Documents/car insurance.pdf", Pdf(false), "Auto insurance policy declarations\nPolicy period 01/01/2025 to 07/01/2025. Vehicle 2019 Subaru Outback. Liability 100/300, collision deductible $500. Premium $612.", 1),
];

/// (search, files that should be found; order among them doesn't matter)
const QUERIES: &[(&str, &[&str])] = &[
    (
        "Show all receipts from Arizona trip on December",
        &[
            "scan_0012.pdf",
            "IMG_20251219_0931.jpg",
            "Elote Cafe.pdf",
            "booking-confirmation.pdf",
        ],
    ),
    (
        "receipts from my Arizona trip",
        &[
            "scan_0012.pdf",
            "IMG_20251219_0931.jpg",
            "Elote Cafe.pdf",
            "booking-confirmation.pdf",
        ],
    ),
    ("what did the hotel in Sedona cost", &["scan_0012.pdf"]),
    ("gas station receipt", &["IMG_20251219_0931.jpg"]),
    (
        "all my receipts",
        &[
            "scan_0012.pdf",
            "IMG_20251219_0931.jpg",
            "Elote Cafe.pdf",
            "booking-confirmation.pdf",
            "costco.pdf",
            "hardware-store.pdf",
        ],
    ),
    ("photos from Sedona", &["IMG_20251220_1102.jpg"]),
    ("apartment rental contract", &["lease.pdf"]),
    ("my tax documents", &["1040-2024.pdf"]),
    ("bill from the web designer", &["acme-invoice-2231.pdf"]),
    (
        "how much money is in my checking account",
        &["bank-statement.pdf"],
    ),
    ("car insurance", &["car insurance.pdf"]),
    ("how to bake bread", &["sourdough.txt"]),
    ("spreadsheet from october", &["household-budget.csv"]),
    (
        "how much did I spend on groceries",
        &["household-budget.csv"],
    ),
    ("the software requirements", &["REQUIREMENTS.md"]),
];

fn month_2025(month: u32) -> SystemTime {
    let t = chrono::Local
        .with_ymd_and_hms(2025, month, 15, 12, 0, 0)
        .unwrap();
    SystemTime::UNIX_EPOCH + Duration::from_secs(t.timestamp() as u64)
}

/// Writes the test files under `root`.
fn make_files(root: &Path) {
    for (path, make, text, month) in DOCS {
        let p = root.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        match make {
            Text => std::fs::write(&p, text).unwrap(),
            Pdf(narrow) => std::fs::write(&p, fixtures::text_pdf(text, *narrow)).unwrap(),
            Scan => std::fs::write(&p, fixtures::scanned_pdf(text)).unwrap(),
            Word => fixtures::docx(&p, text),
            ReceiptPhoto(taken, lat, lon) => std::fs::write(
                &p,
                fixtures::jpeg_with_exif(&fixtures::receipt_photo(text), taken, Some((*lat, *lon))),
            )
            .unwrap(),
            Photo(taken, lat, lon) => std::fs::write(
                &p,
                fixtures::jpeg_with_exif(&fixtures::landscape(), taken, Some((*lat, *lon))),
            )
            .unwrap(),
        }
        std::fs::File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(month_2025(*month))
            .unwrap();
    }
}

/// Writes the test files to a folder to look at:
/// `NINIMMA_EVAL_FILES=<empty dir> cargo test -- --ignored write_eval_files`.
#[test]
#[ignore]
fn write_eval_files() {
    let dir = std::env::var_os("NINIMMA_EVAL_FILES").expect("set NINIMMA_EVAL_FILES");
    make_files(Path::new(&dir));
}

#[test]
#[ignore]
fn finds_documents_by_meaning() {
    assert!(
        std::env::var_os("NINIMMA_TEST_MODEL").is_some(),
        "set NINIMMA_TEST_MODEL"
    );
    let root = tempfile::tempdir().unwrap();
    make_files(root.path());
    // With NINIMMA_TEST_OCR set, the scan and the receipt photo are
    // read too (slow: ~20-35 s each).
    let ocr = std::env::var_os("NINIMMA_TEST_OCR").is_some();
    let settings = IndexSettings {
        enabled: true,
        mode: config::IndexMode::Smart,
        roots: vec![root.path().to_path_buf()],
        pause_on_battery: false,
        ocr,
        worker_threads: 2,
        ..Default::default()
    };
    let store = Arc::new(MemoryKeyStore::default());
    let factory_store = store.clone();
    let dir = tempfile::tempdir().unwrap();
    let mut r = Running {
        service: IndexService::start(
            settings,
            dir.path().join("ninimma/index"),
            Box::new(move || Ok(factory_store as Arc<dyn IndexKeyStore>)),
            Arc::new(|| {}),
        ),
        store,
        dir,
        root,
        last: None,
    };
    // Vectors are made, then (with OCR) the images are read and the text
    // they hold gets vectors too: wait for an embedding pass after OCR.
    let (mut read_images, mut embedded) = (false, false);
    r.wait_longer("vectors", Duration::from_secs(600), |e| {
        if let Event::Status(s) = e {
            read_images |= matches!(s.state, IndexState::ReadingImages { .. });
            embedded |= (read_images || !ocr) && matches!(s.state, IndexState::Embedding { .. });
            return embedded && s.state == IndexState::Ready;
        }
        false
    });

    let (mut total_p3, mut total_rr, mut total_shown) = (0.0, 0.0, 0.0);
    for (text, wanted) in QUERIES {
        let generation = r.service.search(SearchRequest {
            generation: 0,
            text: text.to_string(),
            scope: SearchScope::Everywhere,
            folder: r.root.path().into(),
            offset: 0,
            limit: 20,
            full_text: true,
            smart: true,
            understand: false,
        });
        let event = r.wait_for("results", |e| {
            matches!(e, Event::Results(x) if x.generation == generation)
                || matches!(e, Event::SearchError { generation: g, .. } if *g == generation)
        });
        let found: Vec<String> = match event {
            Event::Results(x) => x.hits.into_iter().map(|h| h.item.name).collect(),
            Event::SearchError { message, .. } => panic!("{text}: {message}"),
            _ => unreachable!(),
        };
        let k = wanted.len().min(3);
        let p3 = found
            .iter()
            .take(k)
            .filter(|n| wanted.contains(&n.as_str()))
            .count() as f64
            / k as f64;
        let rr = found
            .iter()
            .position(|n| wanted.contains(&n.as_str()))
            .map_or(0.0, |i| 1.0 / (i + 1) as f64);
        // Of what's shown (up to 10), how much is relevant: extra results
        // that aren't wanted count against it.
        let shown: Vec<&String> = found.iter().take(10).collect();
        let on_target = if shown.is_empty() {
            0.0
        } else {
            shown
                .iter()
                .filter(|n| wanted.contains(&n.as_str()))
                .count() as f64
                / shown.len() as f64
        };
        total_p3 += p3;
        total_rr += rr;
        total_shown += on_target;
        eprintln!(
            "{p3:.2} {rr:.2} {on_target:.2}  {text:<44} → ({}) {}",
            found.len(),
            found.iter().take(6).cloned().collect::<Vec<_>>().join(", ")
        );
    }
    let n = QUERIES.len() as f64;
    eprintln!(
        "precision@3 {:.2}, MRR {:.2}, relevant share shown {:.2} (OCR {})",
        total_p3 / n,
        total_rr / n,
        total_shown / n,
        if ocr { "on" } else { "off" }
    );
}

/// Prints the model's similarity of each query to each document's text.
#[test]
#[ignore]
fn similarity_table() {
    use crate::embedding::{quantize, similarity, Embedder, Kind};
    let path = std::env::var_os("NINIMMA_TEST_MODEL").expect("set NINIMMA_TEST_MODEL");
    let e = Embedder::at(&crate::models::EMBEDDING_MODEL, path.into());
    let docs: Vec<(&str, String)> = DOCS
        .iter()
        .filter(|d| !d.2.is_empty())
        .map(|d| (d.0.rsplit('/').next().unwrap(), d.2.replace("\\n", "\n")))
        .collect();
    let vectors = e
        .embed(
            &docs.iter().map(|d| d.1.clone()).collect::<Vec<_>>(),
            Kind::Document,
        )
        .unwrap();
    for (q, wanted) in QUERIES {
        let words = crate::index::search::parse(q).unwrap().words.join(" ");
        let qv = &e.embed(std::slice::from_ref(&words), Kind::Query).unwrap()[0];
        let mut scores: Vec<(f32, &str)> = docs
            .iter()
            .zip(&vectors)
            .map(|(d, v)| (similarity(qv, &quantize(v)).unwrap(), d.0))
            .collect();
        scores.sort_by(|a, b| b.0.total_cmp(&a.0));
        let shown: Vec<String> = scores
            .iter()
            .take(6)
            .map(|(s, n)| format!("{}{n} {s:.2}", if wanted.contains(n) { "*" } else { "" }))
            .collect();
        eprintln!("{words:<36} {}", shown.join(", "));
    }
}

/// The test documents' text (as OCR would read the scan and the receipt
/// photo), by file name.
fn texts() -> Vec<(&'static str, String)> {
    DOCS.iter()
        .filter(|d| !d.2.is_empty())
        .map(|d| (d.0.rsplit('/').next().unwrap(), d.2.replace("\\n", "\n")))
        .collect()
}

/// precision@3 and reciprocal rank of a ranking for one query.
fn score(ranked: &[&str], wanted: &[&str]) -> (f64, f64) {
    let k = wanted.len().min(3);
    let p3 = ranked.iter().take(k).filter(|n| wanted.contains(n)).count() as f64 / k as f64;
    let rr = ranked
        .iter()
        .position(|n| wanted.contains(n))
        .map_or(0.0, |i| 1.0 / (i + 1) as f64);
    (p3, rr)
}

/// Compares embedding models and rerankers on the test documents:
/// `NINIMMA_CANDIDATES=<dir of .gguf files> cargo test --release
/// --features smart-search -- --ignored --nocapture compare_models`.
#[test]
#[ignore]
fn compare_models() {
    use crate::embedding::{quantize, similarity, Embedder, Kind};
    let dir =
        PathBuf::from(std::env::var_os("NINIMMA_CANDIDATES").expect("set NINIMMA_CANDIDATES"));
    let docs = texts();
    // (file, SHA-256, query prefix, document prefix, dimensions)
    let embedders = [
        (
            "bge-small-en-v1.5-q8_0.gguf",
            crate::models::EMBEDDING_MODEL.sha256,
            "Represent this sentence for searching relevant passages: ",
            "",
            384,
        ),
        (
            "bge-base-en-v1.5-q8_0.gguf",
            "ad1afe72cd6654a558667a3db10878b049a75bfd72912e1dabb91310d671173c",
            "Represent this sentence for searching relevant passages: ",
            "",
            768,
        ),
        (
            "nomic-embed-text-v1.5.Q8_0.gguf",
            "3e24342164b3d94991ba9692fdc0dd08e3fd7362e0aacc396a9a5c54a544c3b7",
            "search_query: ",
            "search_document: ",
            768,
        ),
    ];
    for (file, sha256, query_prefix, doc_prefix, dimensions) in embedders {
        let path = dir.join(file);
        let spec: &'static crate::models::ModelSpec =
            Box::leak(Box::new(crate::models::ModelSpec {
                id: file,
                sha256,
                size: std::fs::metadata(&path).unwrap().len(),
                dimensions,
                query_prefix,
                ..crate::models::EMBEDDING_MODEL
            }));
        let e = Embedder::at(spec, path);
        e.set_threads(2);
        let started = Instant::now();
        let vectors = e
            .embed(
                &docs
                    .iter()
                    .map(|d| format!("{doc_prefix}{}", d.1))
                    .collect::<Vec<_>>(),
                Kind::Document,
            )
            .unwrap();
        let per_doc = started.elapsed() / docs.len() as u32;
        for full in [false, true] {
            let (mut p, mut r) = (0.0, 0.0);
            for (q, wanted) in QUERIES {
                let text = if full {
                    q.to_string()
                } else {
                    crate::index::search::parse(q).unwrap().words.join(" ")
                };
                let qv = &e.embed(&[text], Kind::Query).unwrap()[0];
                let mut ranked: Vec<(f32, &str)> = docs
                    .iter()
                    .zip(&vectors)
                    .map(|(d, v)| (similarity(qv, &quantize(v)).unwrap(), d.0))
                    .collect();
                ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
                let (p3, rr) = score(&ranked.iter().map(|x| x.1).collect::<Vec<_>>(), wanted);
                p += p3;
                r += rr;
            }
            let n = QUERIES.len() as f64;
            eprintln!(
                "embed  {file:<36} query={:<5} P@3 {:.2} MRR {:.2}  {per_doc:?}/doc",
                if full { "full" } else { "words" },
                p / n,
                r / n
            );
        }
        e.unload();
    }
    for file in [
        "jina-reranker-v1-tiny-en-Q8_0.gguf",
        "bge-reranker-v2-m3-Q4_K_M.gguf",
    ] {
        let mut reranker = Reranker::load(&dir.join(file));
        for full in [false, true] {
            let (mut p, mut r) = (0.0, 0.0);
            let started = Instant::now();
            for (q, wanted) in QUERIES {
                let text = if full {
                    q.to_string()
                } else {
                    crate::index::search::parse(q).unwrap().words.join(" ")
                };
                let mut ranked: Vec<(f32, &str)> = docs
                    .iter()
                    .map(|d| (reranker.score(&text, &d.1), d.0))
                    .collect();
                ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
                let (p3, rr) = score(&ranked.iter().map(|x| x.1).collect::<Vec<_>>(), wanted);
                p += p3;
                r += rr;
            }
            let per_query = started.elapsed() / QUERIES.len() as u32;
            let n = QUERIES.len() as f64;
            eprintln!(
                "rerank {file:<36} query={:<5} P@3 {:.2} MRR {:.2}  {per_query:?}/query of {} docs",
                if full { "full" } else { "words" },
                p / n,
                r / n,
                docs.len()
            );
        }
    }
}

/// A cross-encoder reranker run by llama.cpp (rank pooling), test-only.
struct Reranker {
    context: llama_cpp_2::context::LlamaContext<'static>,
    model: &'static llama_cpp_2::model::LlamaModel,
}

impl Reranker {
    fn load(path: &Path) -> Self {
        use llama_cpp_2::context::params::{LlamaContextParams, LlamaPoolingType};
        let backend = crate::embedding::local::backend().unwrap();
        let params = llama_cpp_2::model::params::LlamaModelParams::default().with_n_gpu_layers(0);
        let model: &'static _ = Box::leak(Box::new(
            llama_cpp_2::model::LlamaModel::load_from_file(backend, path, &params).unwrap(),
        ));
        let size = std::num::NonZeroU32::new(512);
        let cparams = LlamaContextParams::default()
            .with_embeddings(true)
            .with_pooling_type(LlamaPoolingType::Rank)
            .with_n_ctx(size)
            .with_n_batch(512)
            .with_n_ubatch(512)
            .with_n_threads(2)
            .with_n_threads_batch(2);
        let context = model.new_context(backend, cparams).unwrap();
        Self { context, model }
    }

    /// How relevant `doc` is to `query` (higher is better).
    fn score(&mut self, query: &str, doc: &str) -> f32 {
        let vocab = self.model.vocab();
        let mut tokens = vec![vocab.bos()];
        tokens.extend(vocab.tokenize(query.as_bytes(), false, false));
        tokens.push(vocab.eos());
        tokens.push(vocab.sep());
        tokens.extend(vocab.tokenize(doc.as_bytes(), false, false));
        tokens.truncate(511);
        tokens.push(vocab.eos());
        let mut batch = llama_cpp_2::llama_batch::LlamaBatch::new(512, 1);
        batch.add_sequence(&tokens, 0, false).unwrap();
        self.context.clear_kv_cache();
        if self.context.encode(&mut batch).is_err() {
            self.context.clear_kv_cache();
            self.context.decode(&mut batch).unwrap();
        }
        self.context.embeddings_seq_ith(0).unwrap()[0]
    }
}
