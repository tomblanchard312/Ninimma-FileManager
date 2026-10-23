// Ninimma modification: independent project branding and integration.
//! The search index as the UI sees it: starting and stopping the service,
//! sending searches and collecting results, the status-bar text, and the
//! Search Indexing settings dialog.

use crate::file_system::{format_bytes, FileItem};
use crate::index::config::{IndexMode, IndexSettings, MAX_WORKERS};
use crate::index::types::{
    Command, Event, IndexState, IndexStatus, MatchReason, SearchRequest, SearchScope,
};
use crate::index::{search, system, IndexService, KeyStoreFactory};
use crate::models::ModelSpec;
use crate::security::key_store::{IndexKeyStore, OsKeyStore};
use crate::security::permissions;
use crate::security::vault::MIN_PASSPHRASE_CHARS;
use crate::theme::pal;
use crate::ui::FONT_SIZE_SMALL;
use egui::{RichText, Ui};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use zeroize::Zeroizing;

/// What the current indexed search shows.
#[derive(Default)]
pub struct Results {
    /// (text, scope, folder) of the search these results are for.
    pub query: Option<(String, SearchScope, PathBuf)>,
    pub items: Vec<FileItem>,
    pub reasons: HashMap<PathBuf, MatchReason>,
    pub more: bool,
    pub filters: Vec<String>,
    pub error: Option<String>,
    pub searching: bool,
    /// Changes whenever the results do, so the file list rebuilds.
    pub version: u64,
}

/// A model download or install running in the background.
#[derive(Default)]
struct ModelJob {
    progress: Arc<crate::models::Progress>,
    /// Bytes of the files already finished.
    base: Arc<std::sync::atomic::AtomicU64>,
    result: Arc<Mutex<Option<Result<(), String>>>>,
}

pub struct IndexUi {
    service: Option<IndexService>,
    pub status: Option<IndexStatus>,
    /// Turning indexing on: the setting is saved as on only once the index
    /// has been created and opened successfully.
    enabling: bool,
    generation: u64,
    /// Searches include document text (Full text or Smart mode).
    full_text: bool,
    /// Searches include meaning-based matches (Smart mode).
    smart: bool,
    /// Sentences are re-ranked by the experimental query model.
    understand: bool,
    pub results: Results,
    passphrase: Zeroizing<String>,
    confirm: Zeroizing<String>,
    new_root: String,
    new_exclusion: String,
    confirm_delete: bool,
    rebuild_after_delete: bool,
    /// Explaining smart search before it's turned on.
    smart_disclosure: bool,
    /// Model downloads or installs in progress, by model.
    model_jobs: HashMap<&'static str, ModelJob>,
    model_messages: HashMap<&'static str, String>,
    install_paths: HashMap<&'static str, String>,
    network: HashMap<PathBuf, bool>,
    /// Tests use their own folder and an in-memory key store.
    #[cfg(test)]
    pub test_setup: Option<(PathBuf, Arc<crate::security::key_store::MemoryKeyStore>)>,
}

impl Default for IndexUi {
    fn default() -> Self {
        Self {
            service: None,
            status: None,
            enabling: false,
            generation: 0,
            full_text: false,
            smart: false,
            understand: false,
            results: Results::default(),
            passphrase: Zeroizing::new(String::new()),
            confirm: Zeroizing::new(String::new()),
            new_root: String::new(),
            new_exclusion: String::new(),
            confirm_delete: false,
            rebuild_after_delete: false,
            smart_disclosure: false,
            model_jobs: HashMap::new(),
            model_messages: HashMap::new(),
            install_paths: HashMap::new(),
            network: HashMap::new(),
            #[cfg(test)]
            test_setup: None,
        }
    }
}

impl IndexUi {
    fn state(&self) -> Option<&IndexState> {
        self.status.as_ref().map(|s| &s.state)
    }

    /// Whether searches can be answered now.
    pub fn is_searchable(&self) -> bool {
        matches!(
            self.state(),
            Some(
                IndexState::Ready
                    | IndexState::Indexing { .. }
                    | IndexState::Reading { .. }
                    | IndexState::Embedding { .. }
                    | IndexState::ReadingImages { .. }
                    | IndexState::LookingAtPictures { .. }
                    | IndexState::Paused(_)
            )
        )
    }

    /// Connects to the system keyring (tests use their in-memory store).
    fn key_store_factory(&self) -> KeyStoreFactory {
        #[cfg(test)]
        if let Some((_, store)) = self.test_setup.clone() {
            return Box::new(move || Ok(store as Arc<dyn IndexKeyStore>));
        }
        Box::new(|| OsKeyStore::connect().map(|s| Arc::new(s) as Arc<dyn IndexKeyStore>))
    }

    fn start(&mut self, settings: &IndexSettings, ctx: &egui::Context) {
        #[cfg(test)]
        let test = self.test_setup.clone();
        #[cfg(not(test))]
        let test: Option<(PathBuf, Arc<dyn IndexKeyStore>)> = None;

        let dir = match test {
            Some((dir, _)) => dir,
            None => {
                let Some(dir) = permissions::index_dir() else {
                    self.status = Some(IndexStatus {
                        state: IndexState::Failed {
                            message: "There's no per-user folder to keep the index in.".into(),
                            can_rebuild: false,
                        },
                        ..Default::default()
                    });
                    return;
                };
                dir
            }
        };
        let factory = self.key_store_factory();
        let ctx = ctx.clone();
        self.status = Some(IndexStatus::default());
        self.service = Some(IndexService::start(
            settings.clone(),
            dir,
            factory,
            Arc::new(move || ctx.request_repaint()),
        ));
    }

    /// Stops the service (indexing turned off); the index stays on disk.
    fn stop(&mut self) {
        if let Some(s) = self.service.take() {
            s.send(Command::Shutdown);
        }
        self.status = None;
        self.clear_results();
    }

    /// Called every frame: starts or stops the service to match the
    /// settings, and takes in what it reports. Never blocks.
    pub fn poll(&mut self, ctx: &egui::Context, settings: &mut IndexSettings, busy: bool) {
        self.full_text = settings.mode != IndexMode::Basic;
        self.smart = settings.mode == IndexMode::Smart;
        self.understand = settings.understand_sentences && settings.mode != IndexMode::Basic;
        let wanted = settings.enabled || self.enabling;
        if wanted && self.service.is_none() && self.status.is_none() {
            self.start(settings, ctx);
        } else if !wanted && self.service.is_some() {
            self.stop();
        }
        let Some(service) = &self.service else { return };
        service.set_interactive_busy(busy);
        let was_searchable = self.is_searchable();
        let was_busy = matches!(
            self.state(),
            Some(
                IndexState::Indexing { .. }
                    | IndexState::Reading { .. }
                    | IndexState::ReadingImages { .. }
                    | IndexState::LookingAtPictures { .. }
                    | IndexState::Embedding { .. }
            )
        );
        for event in service.poll() {
            match event {
                Event::Status(status) => {
                    if self.enabling
                        && matches!(
                            status.state,
                            IndexState::Ready | IndexState::Indexing { .. }
                        )
                    {
                        // Created and opened: now it's really on.
                        self.enabling = false;
                        settings.enabled = true;
                    }
                    if status.state == IndexState::Deleted {
                        settings.enabled = false;
                        self.service = None;
                        self.clear_results();
                        // Rebuilding: start again (status None restarts it).
                        self.enabling = std::mem::take(&mut self.rebuild_after_delete);
                        self.status = if self.enabling { None } else { Some(status) };
                        break;
                    }
                    self.status = Some(status);
                }
                Event::Results(r) if r.generation == self.generation => {
                    if r.offset == 0 {
                        self.results.items.clear();
                        self.results.reasons.clear();
                    }
                    for hit in r.hits {
                        self.results
                            .reasons
                            .insert(hit.item.path.clone(), hit.reason);
                        self.results.items.push(hit.item);
                    }
                    self.results.more = r.more;
                    self.results.filters = r.filters;
                    self.results.error = None;
                    self.results.searching = false;
                    self.results.version += 1;
                }
                Event::SearchError {
                    generation,
                    message,
                } if generation == self.generation => {
                    self.results.items.clear();
                    self.results.error = Some(message);
                    self.results.searching = false;
                    self.results.version += 1;
                }
                _ => {}
            }
        }
        let now_ready = self.state() == Some(&IndexState::Ready);
        if (!was_searchable && self.is_searchable()) || (was_busy && now_ready) {
            // A search typed while the index was opening, locked or still
            // being built: run it again now that there's more to find.
            self.refresh();
        }
    }

    /// Searches the index, unless this exact search is already shown.
    pub fn search(&mut self, text: &str, scope: SearchScope, folder: &Path) {
        let query = (text.to_string(), scope, folder.to_path_buf());
        if self.results.query.as_ref() == Some(&query) {
            return;
        }
        self.results.query = Some(query);
        // A different search: the last one's results (and why they matched)
        // mustn't stay on screen while this one runs.
        self.results.items.clear();
        self.results.reasons.clear();
        self.results.version += 1;
        self.run_search(text, scope, folder, 0);
    }

    /// Runs the shown search again (the folder changed on disk).
    pub fn refresh(&mut self) {
        if let Some((text, scope, folder)) = self.results.query.clone() {
            self.run_search(&text, scope, &folder, 0);
        }
    }

    pub fn load_more(&mut self) {
        if let Some((text, scope, folder)) = self.results.query.clone() {
            let offset = self.results.items.len();
            self.run_search(&text, scope, &folder, offset);
        }
    }

    fn run_search(&mut self, text: &str, scope: SearchScope, folder: &Path, offset: usize) {
        if !self.is_searchable() {
            self.results.items.clear();
            self.results.error = Some(match self.state() {
                None => {
                    "Turn on search indexing (Edit > Search Indexing) to search subfolders.".into()
                }
                Some(IndexState::NeedsPassphrase { .. }) => {
                    "Unlock the search index to search subfolders.".into()
                }
                _ => "The search index isn't ready yet.".into(),
            });
            self.results.version += 1;
            return;
        }
        let (full_text, smart, understand) = (self.full_text, self.smart, self.understand);
        let Some(service) = &mut self.service else {
            return;
        };
        self.results.searching = true;
        self.results.error = None;
        self.generation = service.search(SearchRequest {
            generation: 0,
            text: text.to_string(),
            scope,
            folder: folder.to_path_buf(),
            offset,
            limit: search::PAGE_SIZE,
            full_text,
            smart,
            understand,
        });
    }

    pub fn clear_results(&mut self) {
        if let Some(s) = &mut self.service {
            s.cancel_search();
        }
        self.results = Results {
            version: self.results.version + 1,
            ..Results::default()
        };
    }

    /// A short note for the status bar; static text, no animation.
    pub fn status_text(&self) -> Option<String> {
        Some(match self.state()? {
            IndexState::Indexing { files } if *files > 0 => format!("Indexing… {files} items"),
            IndexState::Reading { left } => format!("Reading documents… {left} left"),
            IndexState::Embedding { left } => format!("Preparing smart search… {left} left"),
            IndexState::ReadingImages { left } => format!("Reading text in images… {left} left"),
            IndexState::LookingAtPictures { left } => {
                format!("Looking at photos and videos… {left} left")
            }
            IndexState::Indexing { .. } => "Indexing…".into(),
            IndexState::Paused(why) => format!("Indexing paused: {why}"),
            IndexState::NeedsPassphrase { .. } => "Search index locked".into(),
            IndexState::NeedsNewPassphrase { .. } => "Search index needs a passphrase".into(),
            IndexState::Failed { .. } => "Search index unavailable".into(),
            IndexState::Starting | IndexState::Ready | IndexState::Deleted => return None,
        })
    }

    pub fn reason(&self, path: &Path) -> Option<&MatchReason> {
        self.results.reasons.get(path)
    }

    /// The Search Indexing dialog. Returns false when it should close.
    pub fn show_settings(
        &mut self,
        ui: &mut Ui,
        settings: &mut IndexSettings,
        current_folder: &Path,
    ) -> bool {
        let before = settings.clone();
        let mut keep_open = true;
        ui.set_width(560.0);
        ui.heading("Search Indexing");
        ui.label(
            RichText::new(
                "An index lets you search subfolders and all your chosen folders instantly. \
                 It's kept only for you, encrypted, on this computer. Nothing is sent anywhere.",
            )
            .color(pal().text_secondary)
            .size(FONT_SIZE_SMALL),
        );
        ui.add_space(8.0);

        let mut on = settings.enabled || self.enabling;
        if ui.checkbox(&mut on, "Index files for searching").changed() {
            if on {
                if settings.roots.is_empty() {
                    settings.roots = IndexSettings::suggested_roots();
                }
                self.enabling = true;
            } else {
                settings.enabled = false;
                self.enabling = false;
            }
        }

        if let Some(status) = self.status.clone() {
            ui.add_space(6.0);
            self.show_state(ui, &status);
        }

        ui.add_space(8.0);
        egui::CollapsingHeader::new("What to index")
            .default_open(true)
            .show(ui, |ui| {
                let mode_before = settings.mode;
                for mode in [IndexMode::Basic, IndexMode::FullText, IndexMode::Smart] {
                    ui.add_enabled_ui(mode.is_available(), |ui| {
                        let label = if mode.is_available() {
                            mode.label().to_string()
                        } else {
                            format!("{} (not included in this build)", mode.label())
                        };
                        ui.radio_value(&mut settings.mode, mode, label);
                    });
                }
                // Smart search is explained before it's first turned on.
                if settings.mode == IndexMode::Smart
                    && mode_before != IndexMode::Smart
                    && !settings.smart_acknowledged
                {
                    settings.mode = mode_before;
                    self.smart_disclosure = true;
                }
                if self.smart_disclosure {
                    self.show_smart_disclosure(ui, settings);
                }
                if settings.mode == IndexMode::Smart {
                    self.show_models(ui, settings);
                }
                self.show_pictures(ui, settings);
                self.show_ocr(ui, settings);
                self.show_understand(ui, settings);
                if settings.mode != IndexMode::Basic
                    && !crate::index::extractor::pdf_reader_available()
                {
                    ui.label(
                        RichText::new(
                            "PDFs are indexed by name only until Poppler's pdftotext is installed \
                             (poppler-utils on Linux, poppler on FreeBSD and macOS).",
                        )
                        .color(pal().text_secondary)
                        .size(FONT_SIZE_SMALL),
                    );
                }
                ui.add_space(4.0);
                ui.label("Indexed locations");
                let mut remove = None;
                for (i, root) in settings.roots.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui
                            .small_button("✕")
                            .on_hover_text("Stop indexing this folder")
                            .clicked()
                        {
                            remove = Some(i);
                        }
                        ui.label(root.to_string_lossy());
                        let is_network = *self
                            .network
                            .entry(root.clone())
                            .or_insert_with(|| system::is_network_path(root));
                        if is_network {
                            let mut allowed = settings.network_roots.contains(root);
                            if ui.checkbox(&mut allowed, "network share: allow").changed() {
                                if allowed {
                                    settings.network_roots.push(root.clone());
                                } else {
                                    settings.network_roots.retain(|p| p != root);
                                }
                            }
                        }
                    });
                }
                if let Some(i) = remove {
                    let root = settings.roots.remove(i);
                    settings.network_roots.retain(|p| *p != root);
                }
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.new_root)
                            .hint_text("Folder to index")
                            .desired_width(300.0),
                    );
                    if ui.button("Add").clicked() {
                        add_path(&mut settings.roots, &self.new_root);
                        self.new_root.clear();
                    }
                    if ui.button("Add current folder").clicked() {
                        add_path(&mut settings.roots, &current_folder.to_string_lossy());
                    }
                });
                ui.add_space(4.0);
                ui.label("Never index (besides system, cache and credential folders)");
                let mut remove = None;
                for (i, ex) in settings.exclusions.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.small_button("✕").clicked() {
                            remove = Some(i);
                        }
                        ui.label(ex.to_string_lossy());
                    });
                }
                if let Some(i) = remove {
                    settings.exclusions.remove(i);
                }
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.new_exclusion)
                            .hint_text("Folder to leave out")
                            .desired_width(300.0),
                    );
                    if ui.button("Add").clicked() {
                        add_path(&mut settings.exclusions, &self.new_exclusion);
                        self.new_exclusion.clear();
                    }
                });
                ui.checkbox(
                    &mut settings.index_hidden,
                    "Include hidden files and folders",
                );
            });

        egui::CollapsingHeader::new("Resources").show(ui, |ui| {
            ui.checkbox(
                &mut settings.pause_on_battery,
                "Pause indexing on battery power",
            );
            ui.horizontal(|ui| {
                ui.label("Worker threads");
                ui.add(egui::DragValue::new(&mut settings.worker_threads).range(1..=MAX_WORKERS))
                    .on_hover_text("For reading document contents (a later update). Scanning names always uses one thread.");
            });
            ui.horizontal(|ui| {
                ui.label("Maximum index size (MB)");
                ui.add(egui::DragValue::new(&mut settings.max_database_mb).range(16..=8192));
            });
        });

        if settings.roots != before.roots
            || settings.exclusions != before.exclusions
            || settings.network_roots != before.network_roots
            || settings.index_hidden != before.index_hidden
            || settings.pause_on_battery != before.pause_on_battery
            || settings.max_database_mb != before.max_database_mb
            || settings.mode != before.mode
            || settings.max_extract_bytes != before.max_extract_bytes
            || settings.max_semantic_mb != before.max_semantic_mb
            || settings.ocr != before.ocr
        {
            if let Some(s) = &self.service {
                s.send(Command::UpdateSettings(settings.clone()));
            }
        }

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Close").clicked() {
                keep_open = false;
            }
        });
        keep_open
    }

    fn show_state(&mut self, ui: &mut Ui, status: &IndexStatus) {
        match &status.state {
            IndexState::Starting => {
                ui.label("Opening the index…");
            }
            IndexState::NeedsNewPassphrase { error } => {
                ui.label(
                    "This system has no keyring to keep the index key in. Choose a passphrase to protect it \
                     instead; you'll enter it once each time Ninimma starts.",
                );
                ui.horizontal(|ui| {
                    ui.label("Passphrase");
                    ui.add(egui::TextEdit::singleline(&mut *self.passphrase).password(true));
                });
                ui.horizontal(|ui| {
                    ui.label("Again        ");
                    ui.add(egui::TextEdit::singleline(&mut *self.confirm).password(true));
                });
                let long_enough = self.passphrase.chars().count() >= MIN_PASSPHRASE_CHARS;
                let same = *self.passphrase == *self.confirm;
                if !self.passphrase.is_empty() && !long_enough {
                    ui.label(
                        RichText::new(format!("At least {MIN_PASSPHRASE_CHARS} characters."))
                            .color(pal().error),
                    );
                } else if !self.confirm.is_empty() && !same {
                    ui.label(RichText::new("The passphrases don't match.").color(pal().error));
                }
                if let Some(e) = error {
                    ui.label(RichText::new(e.as_str()).color(pal().error));
                }
                ui.label(
                    RichText::new("If you forget it, the index can only be deleted and rebuilt; your files aren't affected.")
                        .color(pal().text_secondary)
                        .size(FONT_SIZE_SMALL),
                );
                if ui
                    .add_enabled(long_enough && same, egui::Button::new("Create index"))
                    .clicked()
                {
                    let pass =
                        std::mem::replace(&mut self.passphrase, Zeroizing::new(String::new()));
                    self.confirm = Zeroizing::new(String::new());
                    if let Some(s) = &self.service {
                        s.send(Command::CreateWithPassphrase(pass));
                    }
                }
            }
            IndexState::NeedsPassphrase { error } => {
                ui.label(
                    "The search index is locked. Enter its passphrase to use it this session.",
                );
                let response =
                    ui.add(egui::TextEdit::singleline(&mut *self.passphrase).password(true));
                if let Some(e) = error {
                    ui.label(RichText::new(e.as_str()).color(pal().error));
                }
                let enter = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (ui.button("Unlock").clicked() || enter) && !self.passphrase.is_empty() {
                    let pass =
                        std::mem::replace(&mut self.passphrase, Zeroizing::new(String::new()));
                    if let Some(s) = &self.service {
                        s.send(Command::Unlock(pass));
                    }
                }
            }
            IndexState::Failed {
                message,
                can_rebuild,
            } => {
                ui.label(RichText::new(message.as_str()).color(pal().error));
                ui.horizontal(|ui| {
                    if ui.button("Try again").clicked() {
                        self.service = None;
                        self.status = None; // restarted next frame
                    }
                    if *can_rebuild && ui.button("Delete and rebuild…").clicked() {
                        self.confirm_delete = true;
                    }
                });
            }
            IndexState::Deleted => {
                ui.label("The index and its key were deleted.");
            }
            IndexState::Ready
            | IndexState::Indexing { .. }
            | IndexState::Reading { .. }
            | IndexState::Embedding { .. }
            | IndexState::ReadingImages { .. }
            | IndexState::LookingAtPictures { .. }
            | IndexState::Paused(_) => {
                let state = match &status.state {
                    IndexState::ReadingImages { left } => {
                        format!("Reading text in images… ({left} left)")
                    }
                    IndexState::LookingAtPictures { left } => {
                        format!("Looking at what photos and videos show… ({left} left)")
                    }
                    IndexState::Embedding { left } => {
                        format!("Preparing smart search… ({left} pieces of text left)")
                    }
                    IndexState::Reading { left } => {
                        format!("Reading documents and photos… ({left} left)")
                    }
                    IndexState::Indexing { files } => {
                        format!("Indexing… ({files} items this pass)")
                    }
                    IndexState::Paused(why) => format!("Paused: {why}"),
                    _ => "Up to date".to_string(),
                };
                ui.label(RichText::new(state).color(pal().accent));
                let last = status
                    .last_scan
                    .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
                    .map(|t| {
                        t.with_timezone(&chrono::Local)
                            .format("%Y-%m-%d %H:%M")
                            .to_string()
                    })
                    .unwrap_or_else(|| "not yet".into());
                ui.label(
                    RichText::new(format!(
                        "{} items · index {} on disk · last full scan {last} · key in {} · {}",
                        status.indexed_files,
                        format_bytes(status.database_bytes),
                        status.key_source.unwrap_or("—"),
                        if status.live_updates {
                            "changes picked up as they happen"
                        } else {
                            "changes picked up by rescans"
                        },
                    ))
                    .color(pal().text_secondary)
                    .size(FONT_SIZE_SMALL),
                );
                if let Some(message) = &status.key_message {
                    ui.label(RichText::new(message.as_str()).size(FONT_SIZE_SMALL));
                }
                if status.key_source == Some("Passphrase") {
                    ui.horizontal(|ui| {
                        if ui
                            .button("Store the key in the system keyring")
                            .on_hover_text(
                                "The index then unlocks when you log in, like other saved passwords \
                                 (GNOME Keyring or KWallet), and the passphrase is no longer needed.",
                            )
                            .clicked()
                        {
                            let factory = self.key_store_factory();
                            if let Some(s) = &self.service {
                                s.send(Command::UseKeyring(factory));
                            }
                        }
                    });
                }
                for (root, why) in &status.unwatched_roots {
                    ui.label(
                        RichText::new(format!("{}: {why}", root.display()))
                            .color(pal().text_secondary)
                            .size(FONT_SIZE_SMALL),
                    );
                }
                for (root, why) in &status.skipped_roots {
                    ui.label(
                        RichText::new(format!("Skipped {}: {why}", root.display()))
                            .color(pal().text_secondary)
                            .size(FONT_SIZE_SMALL),
                    );
                }
                ui.horizontal(|ui| {
                    if let Some(s) = &self.service {
                        let paused = s.is_paused();
                        if ui.button(if paused { "Resume" } else { "Pause" }).clicked() {
                            s.set_paused(!paused);
                        }
                        if ui.button("Rescan now").clicked() {
                            s.send(Command::Rescan);
                        }
                        if ui
                            .button("Rebuild index")
                            .on_hover_text("Delete the index and scan again from scratch")
                            .clicked()
                        {
                            s.send(Command::Rebuild);
                        }
                    }
                    if ui.button("Delete index and key…").clicked() {
                        self.confirm_delete = true;
                    }
                });
            }
        }
        if self.confirm_delete {
            ui.add_space(6.0);
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.label(
                    "Delete the search index and its key? Your own files and folders are not affected; \
                     only the index is removed. You can turn indexing on again later.",
                );
                ui.label(
                    RichText::new(
                        "On SSDs deleted data can't be guaranteed erased, but without the key the old \
                         index can't be decrypted.",
                    )
                    .color(pal().text_secondary)
                    .size(FONT_SIZE_SMALL),
                );
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Delete").color(pal().error)).clicked() {
                        self.confirm_delete = false;
                        self.delete();
                    }
                    if ui.button("Cancel").clicked() {
                        self.confirm_delete = false;
                    }
                });
            });
        }
    }

    fn show_smart_disclosure(&mut self, ui: &mut Ui, settings: &mut IndexSettings) {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.label(RichText::new("About smart search").strong());
            for line in [
                "Smart search finds files by meaning, not just matching words: \"receipts\" can find an invoice.",
                "It runs entirely on this computer with a small open model (BGE small, 37 MB download, MIT \
                 license) through llama.cpp. Nothing is sent anywhere.",
                "It uses the text already read from your documents (text, Word, PowerPoint, spreadsheets, \
                 PDFs) and a few pieces of each, turned into vectors.",
                "The vectors take roughly 1-2 KB per document, up to the size limit below (512 MB by default).",
                "Vectors are encrypted like the rest of the index, but they are derived from your files' \
                 contents. Leave out private folders under \"Never index\".",
            ] {
                ui.label(RichText::new(line).size(FONT_SIZE_SMALL));
            }
            ui.horizontal(|ui| {
                if ui.button("Turn on smart search").clicked() {
                    settings.smart_acknowledged = true;
                    settings.mode = IndexMode::Smart;
                    self.smart_disclosure = false;
                }
                if ui.button("Not now").clicked() {
                    self.smart_disclosure = false;
                }
            });
        });
    }

    /// The smart-search model and the vectors' status.
    fn show_models(&mut self, ui: &mut Ui, settings: &mut IndexSettings) {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            self.model_box(
                ui,
                settings,
                "smart",
                &[&crate::models::EMBEDDING_MODEL],
                "Smart search needs its model.",
            );
            if let Some(status) = &self.status {
                ui.label(
                    RichText::new(format!(
                        "{} vectors · {} on disk",
                        status.vectors,
                        format_bytes(status.semantic_bytes)
                    ))
                    .color(pal().text_secondary)
                    .size(FONT_SIZE_SMALL),
                );
                if let Some(problem) = &status.smart_problem {
                    ui.label(RichText::new(problem.as_str()).color(pal().error));
                }
            }
            ui.horizontal(|ui| {
                ui.label("Maximum size (MB)");
                ui.add(egui::DragValue::new(&mut settings.max_semantic_mb).range(16..=8192));
                if let Some(s) = &self.service {
                    if ui.button("Rebuild smart search").clicked() {
                        s.send(Command::RebuildSemantic);
                    }
                }
            });
        });
    }

    /// Searching what photos and videos show: the switch, what it does,
    /// its model.
    fn show_pictures(&mut self, ui: &mut Ui, settings: &mut IndexSettings) {
        ui.add_enabled_ui(
            settings.mode == IndexMode::Smart && crate::embedding::is_built_in(),
            |ui| {
                ui.checkbox(
                    &mut settings.pictures,
                    "Search photos and videos by what they show (\"mountains\", \"a cat\")",
                );
            },
        );
        if !settings.pictures || settings.mode != IndexMode::Smart {
            return;
        }
        egui::Frame::group(ui.style()).show(ui, |ui| {
            for line in [
                "Photos and a few frames of each video are looked at in the background, so a search like \
                 \"photos of mountains\" or \"video of a cat\" finds them without those words in their names. \
                 It runs on this computer only, with OpenAI's CLIP model (MIT).",
                "About 40 ms a photo on a recent CPU (a 10,000-photo library in well under an hour). While it \
                 works, Ninimma uses about 700 MB more memory; the model is unloaded when it's done.",
                "What each picture shows is kept as numbers, encrypted with the rest of the index; no \
                 descriptions are written.",
            ] {
                ui.label(RichText::new(line).size(FONT_SIZE_SMALL));
            }
            self.model_box(ui, settings, "pictures", &crate::models::PICTURE_FILES, "Searching pictures needs its model.");
            if let Some(status) = self.status.as_ref() {
                if let Some(problem) = &status.picture_problem {
                    ui.label(RichText::new(problem.as_str()).color(pal().error));
                } else if status.pictures > 0 {
                    ui.label(
                        RichText::new(format!("{} photos and videos searchable by what they show.", status.pictures))
                            .color(pal().text_secondary)
                            .size(FONT_SIZE_SMALL),
                    );
                }
            }
            if !crate::thumbnail::can_read_videos() {
                ui.label(
                    RichText::new("Videos need FFmpeg; photos work without it.")
                        .color(pal().text_secondary)
                        .size(FONT_SIZE_SMALL),
                );
            }
        });
    }

    /// Reading text in images (OCR): the switch, an explanation, its model.
    fn show_ocr(&mut self, ui: &mut Ui, settings: &mut IndexSettings) {
        ui.add_enabled_ui(
            settings.mode != IndexMode::Basic && crate::embedding::is_built_in(),
            |ui| {
                ui.checkbox(
                    &mut settings.ocr,
                    "Read text in photos of documents and in scanned PDFs (OCR)",
                );
            },
        );
        if !settings.ocr || settings.mode == IndexMode::Basic {
            return;
        }
        egui::Frame::group(ui.style()).show(ui, |ui| {
            for line in [
                "Photographed receipts, letters and scans become searchable by what's printed on them. \
                 It runs on this computer only, with IBM's granite-docling model (Apache-2.0).",
                "Only images that look like paper are read, in the background: about 35 seconds each on one \
                 CPU thread, 20 on two. Other photos are skipped in a moment. While it reads, Ninimma \
                 uses about 500 MB more memory; the model is unloaded when it's done.",
                "The text read is stored encrypted with the rest of the index.",
            ] {
                ui.label(RichText::new(line).size(FONT_SIZE_SMALL));
            }
            self.model_box(ui, settings, "ocr", &crate::models::OCR_FILES, "Reading text in images needs its model.");
            if let Some(problem) = self.status.as_ref().and_then(|s| s.ocr_problem.as_ref()) {
                ui.label(RichText::new(problem.as_str()).color(pal().error));
            }
            if crate::ocr::find_tool(if cfg!(windows) { "pdftoppm.exe" } else { "pdftoppm" }).is_none() {
                ui.label(
                    RichText::new("Scanned PDFs need Poppler's pdftoppm (poppler-utils / poppler); photos work without it.")
                        .color(pal().text_secondary)
                        .size(FONT_SIZE_SMALL),
                );
            }
        });
    }

    /// The experimental query model: the switch, what it does, its model.
    fn show_understand(&mut self, ui: &mut Ui, settings: &mut IndexSettings) {
        ui.add_enabled_ui(
            settings.mode != IndexMode::Basic && crate::embedding::is_built_in(),
            |ui| {
                ui.checkbox(
                    &mut settings.understand_sentences,
                    "Understand searches written as sentences with a small local model (experimental)",
                );
            },
        );
        if !settings.understand_sentences || settings.mode == IndexMode::Basic {
            return;
        }
        egui::Frame::group(ui.style()).show(ui, |ui| {
            for line in [
                "For searches like \"receipts from my Arizona trip in December\", the model picks out the \
                 words that matter most, and files matching them move up. It never adds or removes results: \
                 dates, places and file types are always read by the built-in rules.",
                "Results appear straight away and are re-ranked when the model answers, about 2 seconds \
                 later. It runs on this computer only (Qwen2.5 0.5B, Apache-2.0). While loaded it uses about \
                 500 MB of memory; it's unloaded a minute after the last search. It can misread a sentence.",
            ] {
                ui.label(RichText::new(line).size(FONT_SIZE_SMALL));
            }
            self.model_box(
                ui,
                settings,
                "query",
                &[&crate::models::QUERY_MODEL],
                "Understanding sentences needs its model.",
            );
        });
    }

    /// A model's status, download progress and install/remove buttons.
    /// `key` tells models apart; `specs` are all the files it needs.
    fn model_box(
        &mut self,
        ui: &mut Ui,
        settings: &IndexSettings,
        key: &'static str,
        specs: &[&'static ModelSpec],
        needs: &str,
    ) {
        use crate::models;
        use std::sync::atomic::Ordering;
        // A finished download or install.
        if let Some(result) = self
            .model_jobs
            .get(key)
            .and_then(|j| j.result.lock().unwrap().take())
        {
            self.model_jobs.remove(key);
            self.model_messages.insert(
                key,
                match result {
                    Ok(()) => "Model installed.".to_string(),
                    Err(e) => format!("Couldn't install the model: {e}."),
                },
            );
            if let Some(s) = &self.service {
                // Wakes the indexer to start using it.
                s.send(Command::UpdateSettings(settings.clone()));
            }
        }
        let total: u64 = specs.iter().map(|s| s.size).sum();
        let main = specs[0];
        ui.horizontal(|ui| {
            ui.label(format!(
                "Model: {} · {} · {} license ·",
                main.name,
                format_bytes(total),
                main.license
            ));
            ui.hyperlink_to("about", main.source);
        });
        let installed = specs.iter().all(|s| models::is_installed(s));
        if let Some(job) = self.model_jobs.get(key) {
            let done = job.base.load(Ordering::Relaxed) + job.progress.done.load(Ordering::Relaxed);
            ui.horizontal(|ui| {
                ui.add(
                    egui::ProgressBar::new(done as f32 / total as f32)
                        .desired_width(240.0)
                        .show_percentage(),
                );
                if ui.button("Cancel").clicked() {
                    job.progress.cancel.store(true, Ordering::Relaxed);
                }
            });
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(250));
        } else if installed {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Installed").color(pal().accent));
                if ui.small_button("Remove model").clicked() {
                    for s in specs {
                        let _ = models::remove(s);
                    }
                }
            });
        } else {
            ui.label(
                RichText::new(format!(
                    "{needs} Download it (once, from Hugging Face; checked against its published checksum), \
                     or install a copy you downloaded elsewhere."
                ))
                .size(FONT_SIZE_SMALL),
            );
            let path = self.install_paths.entry(key).or_default();
            let mut start = None;
            ui.horizontal(|ui| {
                if ui
                    .button(format!("Download ({})", format_bytes(total)))
                    .clicked()
                {
                    start = Some(None);
                }
                let hint = if specs.len() == 1 {
                    specs[0].file
                } else {
                    "folder with the .gguf files"
                };
                ui.add(
                    egui::TextEdit::singleline(path)
                        .hint_text(hint)
                        .desired_width(220.0),
                );
                if ui.button("Install from file").clicked() && !path.trim().is_empty() {
                    start = Some(Some(crate::ui::expand_home(path.trim())));
                }
            });
            if let Some(from) = start {
                self.start_model_job(ui.ctx(), key, specs, from);
            }
            if let Some(dir) = models::models_dir() {
                let files: Vec<&str> = specs.iter().map(|s| s.file).collect();
                ui.label(
                    RichText::new(format!(
                        "Without internet: copy {} into {}",
                        files.join(" and "),
                        dir.display()
                    ))
                    .color(pal().text_secondary)
                    .size(FONT_SIZE_SMALL),
                );
            }
        }
        if let Some(m) = self.model_messages.get(key) {
            ui.label(RichText::new(m.as_str()).size(FONT_SIZE_SMALL));
        }
    }

    /// Downloads a model's files, or installs them from `from` (a file, or
    /// a folder holding them), in the background.
    fn start_model_job(
        &mut self,
        ctx: &egui::Context,
        key: &'static str,
        specs: &[&'static ModelSpec],
        from: Option<PathBuf>,
    ) {
        use crate::models;
        use std::sync::atomic::Ordering;
        let job = ModelJob::default();
        let (progress, base, result, ctx) = (
            job.progress.clone(),
            job.base.clone(),
            job.result.clone(),
            ctx.clone(),
        );
        let specs: Vec<&'static ModelSpec> = specs.to_vec();
        std::thread::spawn(move || {
            let mut outcome = Ok(());
            for spec in specs {
                if models::is_installed(spec) {
                    base.fetch_add(spec.size, Ordering::Relaxed);
                    continue;
                }
                let step = match &from {
                    Some(path) if path.is_dir() => {
                        models::install_from(spec, &path.join(spec.file))
                    }
                    Some(path) => models::install_from(spec, path),
                    None => models::download(spec, &progress),
                };
                if let Err(e) = step {
                    outcome = Err(e.to_string());
                    break;
                }
                base.fetch_add(spec.size, Ordering::Relaxed);
                progress.done.store(0, Ordering::Relaxed);
            }
            *result.lock().unwrap() = Some(outcome);
            ctx.request_repaint();
        });
        self.model_messages.remove(key);
        self.model_jobs.insert(key, job);
    }

    fn delete(&mut self) {
        if let Some(s) = &self.service {
            // After a failure, "Delete and rebuild" starts a fresh index.
            self.rebuild_after_delete = matches!(self.state(), Some(IndexState::Failed { .. }));
            s.send(Command::DeleteIndexAndKey);
        }
    }
}

fn add_path(list: &mut Vec<PathBuf>, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let path = crate::ui::expand_home(text);
    if path.is_absolute() && !list.contains(&path) {
        list.push(path);
    }
}
