// Ninimma modification: independent UI harness, smart-search demo and egui compatibility.
//! End-to-end UI tests: drive the real UI with simulated mouse and keyboard
//! input via egui_kittest (no window or GPU needed, so they run in CI).

use super::*;
use egui::{MouseWheelUnit, TouchPhase};
use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;
use image::{ImageDecoder, ImageReader};

struct App {
    ui: NinimmaUI,
    fs: FileSystem,
    _cache: tempfile::TempDir,
}

/// A folder of five photos plus a text file, which opens in Icons view.
fn picture_folder() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for i in 1..=5 {
        image::DynamicImage::new_rgb8(40, 20)
            .save(dir.path().join(format!("IMG_{i}.jpg")))
            .unwrap();
    }
    std::fs::write(dir.path().join("notes.txt"), "hello").unwrap();
    dir
}

fn harness(dir: &Path) -> Harness<'static, App> {
    harness_sized(dir, Vec2::new(1400.0, 900.0))
}

fn harness_sized(dir: &Path, size: Vec2) -> Harness<'static, App> {
    let cache = tempfile::tempdir().unwrap();
    let fs = FileSystem::new(dir.to_path_buf(), false);
    let mut ui = NinimmaUI::with_thumbnails(
        Settings::default(),
        &fs,
        ThumbnailCache::with_cache_dir(cache.path().into()),
    );
    // Don't start the background drive/network refresh: it would replace
    // places a test sets up with whatever this machine really has, at a
    // random moment (it made a sidebar test flaky on Windows CI).
    ui.drives_thread_started = true;
    // Never touch the real (Nemo-shared) bookmarks file from tests.
    ui.bookmarks_file = Some(cache.path().join("bookmarks"));
    ui.dolphin_places_file = None;
    ui.templates = Vec::new();
    ui.menus_refreshed = Some(std::time::Instant::now() + Duration::from_secs(3600));
    let mut h = Harness::builder()
        .with_context_setup(crate::configure_style)
        .with_size(size)
        // Real frame timing, so double-click detection (300 ms) behaves as in the app.
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, app: &mut App| app.ui.show(ui, &mut app.fs),
            App {
                ui,
                fs,
                _cache: cache,
            },
        );
    h.run_steps(3);
    h
}

fn ctrl_wheel(h: &mut Harness<'static, App>, lines: f32) {
    h.hover_at(Pos2::new(700.0, 450.0));
    h.event_modifiers(
        Event::MouseWheel {
            source: egui::MouseWheelSource::Wheel,
            unit: MouseWheelUnit::Line,
            delta: Vec2::new(0.0, lines),
            phase: TouchPhase::Move,
            modifiers: Modifiers::COMMAND,
        },
        Modifiers::COMMAND,
    );
    h.run_steps(20); // egui smooths wheel input over several frames
}

/// Step until background jobs (rotate, paste, ...) are done.
fn wait_for_jobs(h: &mut Harness<'static, App>) {
    for _ in 0..500 {
        h.step();
        if h.state().ui.jobs.is_empty() {
            h.run_steps(2);
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("background job did not finish");
}

#[test]
fn picture_folders_open_in_large_icons() {
    let dir = picture_folder();
    let h = harness(dir.path());
    assert_eq!(h.state().ui.view.mode, ViewMode::Icons);
}

#[test]
fn ctrl_mouse_wheel_resizes_icons_and_walks_through_views() {
    let dir = picture_folder();
    let mut h = harness(dir.path());
    let start = h.state().ui.view.icon_size;

    ctrl_wheel(&mut h, 2.0); // wheel up = bigger
    let bigger = h.state().ui.view.icon_size;
    assert!(bigger > start, "{bigger} should be > {start}");

    ctrl_wheel(&mut h, -4.0); // wheel down = smaller
    assert!(h.state().ui.view.icon_size < bigger);

    // Keep shrinking: icons -> tiles -> details.
    for _ in 0..12 {
        ctrl_wheel(&mut h, -3.0);
    }
    assert_eq!(h.state().ui.view.mode, ViewMode::Details);

    // And back up again.
    for _ in 0..3 {
        ctrl_wheel(&mut h, 3.0);
    }
    assert_eq!(h.state().ui.view.mode, ViewMode::Icons);

    // The view is remembered for this folder.
    let remembered = h
        .state()
        .ui
        .settings
        .folder_views
        .get(dir.path())
        .map(|(v, _)| *v);
    assert_eq!(remembered, Some(h.state().ui.view));
}

#[test]
fn plain_wheel_scrolls_instead_of_zooming() {
    let dir = picture_folder();
    let mut h = harness(dir.path());
    let size = h.state().ui.view.icon_size;
    h.hover_at(Pos2::new(700.0, 450.0));
    h.event(Event::MouseWheel {
        source: egui::MouseWheelSource::Wheel,
        unit: MouseWheelUnit::Line,
        delta: Vec2::new(0.0, -3.0),
        phase: TouchPhase::Move,
        modifiers: Modifiers::NONE,
    });
    h.run_steps(20);
    assert_eq!(h.state().ui.view.icon_size, size);
}

#[test]
fn click_ctrl_click_and_shift_click_select_items() {
    let dir = picture_folder();
    let mut h = harness(dir.path());

    h.get_by_label("IMG_1.jpg").click();
    h.run_steps(2);
    assert_eq!(h.state().ui.selection.len(), 1);

    h.get_by_label("IMG_3.jpg")
        .click_modifiers(Modifiers::COMMAND);
    h.run_steps(2);
    assert_eq!(h.state().ui.selection.len(), 2);

    h.get_by_label("IMG_5.jpg")
        .click_modifiers(Modifiers::SHIFT);
    h.run_steps(2);
    // Range from the anchor (IMG_3) to IMG_5, replacing the earlier selection.
    let selected: Vec<String> = h
        .state()
        .ui
        .selected_paths()
        .iter()
        .map(|p| display_name(p))
        .collect();
    assert_eq!(selected, ["IMG_3.jpg", "IMG_4.jpg", "IMG_5.jpg"]);
}

#[test]
fn bulk_rotate_selected_pictures_from_the_toolbar() {
    let dir = picture_folder();
    let mut h = harness(dir.path());

    // Rotate is disabled until rotatable pictures are selected.
    h.get_by_label("notes.txt").click();
    h.run_steps(2);
    assert!(h.state().ui.rotatable_selection().is_none());

    for i in 1..=4 {
        h.get_by_label(&format!("IMG_{i}.jpg"))
            .click_modifiers(if i == 1 {
                Modifiers::NONE
            } else {
                Modifiers::COMMAND
            });
        h.run_steps(1);
    }
    h.get_by_label("⟳ Rotate right").click();
    h.run_steps(1);
    wait_for_jobs(&mut h);

    let orientation = |name: &str| {
        let mut dec = ImageReader::open(dir.path().join(name))
            .unwrap()
            .into_decoder()
            .unwrap();
        dec.orientation().unwrap()
    };
    for i in 1..=4 {
        assert_eq!(
            orientation(&format!("IMG_{i}.jpg")),
            image::metadata::Orientation::Rotate90
        );
    }
    assert_eq!(
        orientation("IMG_5.jpg"),
        image::metadata::Orientation::NoTransforms
    );

    // Rotate left twice undoes one right turn and adds a left one.
    h.get_by_label("⟲ Rotate left").click();
    h.run_steps(1);
    wait_for_jobs(&mut h);
    assert_eq!(
        orientation("IMG_1.jpg"),
        image::metadata::Orientation::NoTransforms
    );
}

#[test]
fn delete_and_backspace_while_typing_never_touch_files() {
    let dir = picture_folder();
    let mut h = harness(dir.path());
    h.get_by_label("IMG_1.jpg").click();
    h.run_steps(2);

    // Focus the search box (Ctrl+F) and press the dangerous keys.
    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    h.run_steps(2);
    h.event(Event::Text("IMG".into()));
    h.run_steps(2);
    h.key_press(Key::Backspace);
    h.key_press(Key::Delete);
    h.run_steps(3);

    assert!(dir.path().join("IMG_1.jpg").exists());
    assert!(
        h.state().ui.jobs.is_empty(),
        "no delete job should have started"
    );
    assert_eq!(
        h.state().fs.get_current_path(),
        dir.path(),
        "backspace must not navigate while typing"
    );
    assert_eq!(h.state().ui.search_text, "IM");
    assert_eq!(
        h.state().ui.list.items.len(),
        5,
        "search filters to the pictures"
    );
}

#[test]
fn keyboard_navigation_and_select_all() {
    let dir = picture_folder();
    let mut h = harness(dir.path());
    h.key_press(Key::ArrowRight);
    h.key_press(Key::ArrowRight);
    h.run_steps(2);
    let focus = h.state().ui.selection.focus().map(display_name);
    assert_eq!(focus.as_deref(), Some("IMG_2.jpg"));

    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.run_steps(2);
    assert_eq!(h.state().ui.selection.len(), 6);

    h.key_press(Key::Escape);
    h.run_steps(2);
    assert!(h.state().ui.selection.is_empty());
}

#[test]
fn shift_arrows_extend_and_shrink_a_contiguous_range_in_every_view() {
    let dir = picture_folder();
    let mut h = harness(dir.path());
    let names = |h: &Harness<'static, App>| -> Vec<String> {
        h.state()
            .ui
            .selected_paths()
            .iter()
            .map(|p| display_name(p))
            .collect()
    };
    // Details moves by rows; Tiles and Icons are grids, so step with Left/Right.
    for (mode, next, prev) in [
        (ViewMode::Details, Key::ArrowDown, Key::ArrowUp),
        (ViewMode::Tiles, Key::ArrowRight, Key::ArrowLeft),
        (ViewMode::Icons, Key::ArrowRight, Key::ArrowLeft),
    ] {
        h.state_mut().ui.view.mode = mode;
        h.run_steps(2);
        h.get_by_label("IMG_1.jpg").click();
        h.run_steps(2);
        for _ in 0..3 {
            h.key_press_modifiers(Modifiers::SHIFT, next);
        }
        h.run_steps(2);
        assert_eq!(
            names(&h),
            ["IMG_1.jpg", "IMG_2.jpg", "IMG_3.jpg", "IMG_4.jpg"],
            "{mode:?}"
        );

        h.key_press_modifiers(Modifiers::SHIFT, prev);
        h.run_steps(2);
        assert_eq!(
            names(&h),
            ["IMG_1.jpg", "IMG_2.jpg", "IMG_3.jpg"],
            "{mode:?}"
        );

        h.key_press_modifiers(Modifiers::SHIFT, Key::End);
        h.run_steps(2);
        assert_eq!(h.state().ui.selection.len(), 6, "{mode:?}");

        h.key_press_modifiers(Modifiers::SHIFT, Key::Home);
        h.run_steps(2);
        assert_eq!(names(&h), ["IMG_1.jpg"], "{mode:?}");

        // A plain arrow still moves and collapses the selection.
        h.key_press(next);
        h.run_steps(2);
        assert_eq!(names(&h), ["IMG_2.jpg"], "{mode:?}");
    }
}

#[test]
fn copy_paste_creates_suffixed_duplicates() {
    let dir = picture_folder();
    let mut h = harness(dir.path());
    h.get_by_label("notes.txt").click();
    h.run_steps(2);
    let mut input = egui::RawInput::default();
    input.events.push(Event::Key {
        key: Key::C,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::COMMAND,
    });
    crate::system_clipboard::prepare_input(&h.ctx, &mut input);
    for event in input.events {
        h.event(event);
    }
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND, Key::V);
    h.run_steps(1);
    wait_for_jobs(&mut h);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("notes - Copy.txt")).unwrap(),
        "hello"
    );
}

#[test]
fn copy_puts_files_on_the_system_clipboard_for_other_apps() {
    let dir = picture_folder();
    let mut h = harness(dir.path());
    h.get_by_label("notes.txt").click();
    h.run_steps(2);
    h.event(Event::Copy);
    h.run_steps(2);
    assert_eq!(
        crate::system_clipboard::files(),
        Some(vec![dir.path().join("notes.txt")])
    );
}

#[test]
fn files_copied_in_another_app_paste_here() {
    let dir = picture_folder();
    let elsewhere = tempfile::tempdir().unwrap();
    let report = elsewhere.path().join("report.pdf");
    std::fs::write(&report, "pdf").unwrap();
    let mut h = harness(dir.path());

    // Our own copy first, then another file manager copies something else.
    h.get_by_label("notes.txt").click();
    h.run_steps(2);
    h.event(Event::Copy);
    h.run_steps(2);
    crate::system_clipboard::copied_elsewhere(&[report.as_path()]);
    h.key_press_modifiers(Modifiers::COMMAND, Key::V);
    h.run_steps(1);
    wait_for_jobs(&mut h);

    assert!(
        dir.path().join("report.pdf").exists(),
        "the newer copy wins"
    );
    assert!(report.exists(), "a copy, not a move");
    assert!(!dir.path().join("notes - Copy.txt").exists());
}

#[test]
fn new_folder_opens_rename_and_mouse_back_button_navigates() {
    let dir = picture_folder();
    let sub = dir.path().join("Sub");
    std::fs::create_dir(&sub).unwrap();
    let mut h = harness(dir.path());

    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::N);
    h.run_steps(3);
    assert!(dir.path().join("New folder").is_dir());
    assert!(matches!(h.state().ui.dialog, Some(Dialog::Rename { .. })));
    h.key_press(Key::Escape);
    h.run_steps(3);
    assert!(
        h.state().ui.dialog.is_none(),
        "Escape cancels the rename dialog"
    );

    h.get_by_label("Sub").click();
    h.step();
    h.get_by_label("Sub").click();
    h.run_steps(3);
    assert_eq!(h.state().fs.get_current_path(), sub);

    h.event(Event::PointerButton {
        pos: Pos2::new(700.0, 450.0),
        button: PointerButton::Extra1,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    h.run_steps(3);
    assert_eq!(h.state().fs.get_current_path(), dir.path());
    // Coming back selects the folder we were in.
    assert!(h.state().ui.selection.contains(&sub));
}

#[test]
fn enter_that_confirms_rename_does_not_also_open_the_file() {
    let dir = picture_folder();
    let mut h = harness(dir.path());
    h.get_by_label("notes.txt").click();
    h.run_steps(2);
    h.key_press(Key::F2);
    h.run_steps(3);
    // The name without extension is pre-selected, so typing replaces it.
    h.event(Event::Text("todo".into()));
    h.run_steps(2);
    h.key_press(Key::Enter);
    h.run_steps(4);

    assert!(dir.path().join("todo.txt").exists());
    assert!(h.state().ui.dialog.is_none());
    assert!(
        h.state().ui.opened.is_empty(),
        "Enter must not open {:?}",
        h.state().ui.opened
    );
}

#[test]
fn enter_in_address_bar_navigates_without_opening_selection() {
    let dir = picture_folder();
    let sub = dir.path().join("Sub");
    std::fs::create_dir(&sub).unwrap();
    let mut h = harness(dir.path());
    h.get_by_label("notes.txt").click();
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND, Key::L);
    h.run_steps(3);
    // Address text is pre-selected, so typing replaces it.
    h.event(Event::Text(sub.to_string_lossy().to_string()));
    h.run_steps(2);
    h.key_press(Key::Enter);
    h.run_steps(4);

    assert_eq!(h.state().fs.get_current_path(), sub);
    assert!(h.state().ui.opened.is_empty());
}

#[test]
fn address_clipboard_events_edit_text_without_copying_selected_files() {
    let dir = picture_folder();
    let mut h = harness(dir.path());
    h.get_by_label("notes.txt").click();
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND, Key::L);
    h.run_steps(3);
    h.event(Event::Copy);
    h.step();
    assert!(h.output().platform_output.commands.iter().any(
        |c| matches!(c, egui::OutputCommand::CopyText(t) if t == &dir.path().to_string_lossy())
    ));
    assert!(h.state().ui.clipboard.is_none());
    h.event(Event::Paste("/tmp/pasted address".into()));
    h.step();
    assert_eq!(h.state().ui.address_text, "/tmp/pasted address");
    assert_eq!(h.state().fs.get_current_path(), dir.path());
    assert!(h.state().ui.jobs.is_empty());
}

#[test]
fn quick_click_elsewhere_then_double_click_still_opens() {
    // egui would call this a triple-click; it must still open the folder.
    let dir = picture_folder();
    let sub = dir.path().join("Sub");
    std::fs::create_dir(&sub).unwrap();
    let mut h = harness(dir.path());
    h.get_by_label("IMG_1.jpg").click();
    h.step();
    h.get_by_label("Sub").click();
    h.step();
    h.get_by_label("Sub").click();
    h.run_steps(3);
    assert_eq!(h.state().fs.get_current_path(), sub);
}

#[test]
fn double_click_opens_files_in_default_app() {
    let dir = picture_folder();
    let mut h = harness(dir.path());
    h.get_by_label("notes.txt").click();
    h.step();
    h.get_by_label("notes.txt").click();
    h.run_steps(2);
    assert_eq!(h.state().ui.opened, [dir.path().join("notes.txt")]);
}

#[derive(Debug)]
struct TestDrop(PathBuf);

impl egui::DroppedFile for TestDrop {
    fn path(&self) -> &Path {
        &self.0
    }
    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

fn drop_files(h: &mut Harness<'static, App>, paths: &[PathBuf]) {
    for p in paths {
        h.input_mut()
            .dropped_files
            .push(Arc::new(TestDrop(p.clone())));
    }
    h.run_steps(1);
    wait_for_jobs(h);
}

#[test]
fn dragging_an_item_starts_an_os_drag_of_the_selection() {
    let dir = picture_folder();
    let mut h = harness(dir.path());
    h.get_by_label("IMG_1.jpg").click();
    h.step();
    h.get_by_label("IMG_2.jpg")
        .click_modifiers(Modifiers::COMMAND);
    h.step();

    let start = h.get_by_label("IMG_2.jpg").rect().center();
    h.event(Event::PointerMoved(start));
    h.event(Event::PointerButton {
        pos: start,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    for dx in [5.0, 15.0, 30.0, 60.0] {
        h.event(Event::PointerMoved(start + Vec2::new(dx, 0.0)));
        h.step();
    }
    let (paths, move_files) = h.state_mut().ui.take_drag_out().expect("drag should start");
    let names: Vec<String> = paths.iter().map(|p| display_name(p)).collect();
    assert_eq!(names, ["IMG_1.jpg", "IMG_2.jpg"]);
    assert!(!move_files);
}

#[test]
fn files_dropped_on_the_window_move_in_but_same_folder_drops_do_nothing() {
    let dir = picture_folder();
    let elsewhere = tempfile::tempdir_in(dir.path().parent().unwrap()).unwrap();
    let incoming = elsewhere.path().join("incoming.txt");
    std::fs::write(&incoming, "hi").unwrap();
    let mut h = harness(dir.path());

    // Same drive: moves.
    drop_files(&mut h, std::slice::from_ref(&incoming));
    assert!(dir.path().join("incoming.txt").exists());
    assert!(!incoming.exists());

    // Dropping a file onto the folder it's already in is a no-op (no "- Copy").
    drop_files(&mut h, &[dir.path().join("notes.txt")]);
    assert!(!dir.path().join("notes - Copy.txt").exists());
}

#[test]
fn drop_modifiers_override_the_default() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("a");
    std::fs::write(&f, "x").unwrap();
    assert_eq!(drop_mode(&f, dir.path(), Modifiers::NONE), PasteMode::Move);
    assert_eq!(
        drop_mode(&f, dir.path(), Modifiers::COMMAND),
        PasteMode::Copy
    );
    assert_eq!(
        drop_mode(&f, Path::new("/nonexistent"), Modifiers::SHIFT),
        PasteMode::Move
    );
}

#[test]
fn ctrl_z_undoes_rename_rotate_and_paste_and_ctrl_y_redoes() {
    let dir = picture_folder();
    let mut h = harness(dir.path());

    // Rename via F2.
    h.get_by_label("notes.txt").click();
    h.run_steps(2);
    h.key_press(Key::F2);
    h.run_steps(3);
    h.event(Event::Text("todo".into()));
    h.key_press(Key::Enter);
    h.run_steps(3);
    assert!(dir.path().join("todo.txt").exists());

    // Rotate a picture.
    h.get_by_label("IMG_1.jpg").click();
    h.run_steps(2);
    h.get_by_label("⟳ Rotate right").click();
    h.run_steps(1);
    wait_for_jobs(&mut h);
    let orientation = |name: &str| {
        let mut dec = ImageReader::open(dir.path().join(name))
            .unwrap()
            .into_decoder()
            .unwrap();
        dec.orientation().unwrap()
    };
    assert_eq!(
        orientation("IMG_1.jpg"),
        image::metadata::Orientation::Rotate90
    );

    // Undo the rotation, then the rename.
    h.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    h.run_steps(1);
    wait_for_jobs(&mut h);
    assert_eq!(
        orientation("IMG_1.jpg"),
        image::metadata::Orientation::NoTransforms
    );
    h.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    h.run_steps(1);
    wait_for_jobs(&mut h);
    assert!(dir.path().join("notes.txt").exists() && !dir.path().join("todo.txt").exists());

    // Redo brings the rename back.
    h.key_press_modifiers(Modifiers::COMMAND, Key::Y);
    h.run_steps(1);
    wait_for_jobs(&mut h);
    assert!(dir.path().join("todo.txt").exists());

    // Copy + paste, then undo removes the copy.
    h.get_by_label("todo.txt").click();
    h.run_steps(2);
    h.event(Event::Copy);
    h.run_steps(1);
    h.key_press_modifiers(Modifiers::COMMAND, Key::V);
    h.run_steps(1);
    wait_for_jobs(&mut h);
    assert!(dir.path().join("todo - Copy.txt").exists());
    h.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    h.run_steps(1);
    wait_for_jobs(&mut h);
    assert!(!dir.path().join("todo - Copy.txt").exists());
    assert!(dir.path().join("todo.txt").exists());
}

#[test]
fn pasting_onto_existing_names_asks_replace_skip_or_keep_both() {
    let dir = picture_folder();
    let other = tempfile::tempdir_in(dir.path().parent().unwrap()).unwrap();
    std::fs::write(other.path().join("notes.txt"), "incoming").unwrap();
    std::fs::write(other.path().join("fresh.txt"), "new file").unwrap();
    let mut h = harness(dir.path());

    let paste = |h: &mut Harness<'static, App>| {
        let text = [
            other.path().join("notes.txt"),
            other.path().join("fresh.txt"),
        ]
        .map(|p| p.to_string_lossy().to_string())
        .join("\n");
        h.event(Event::Paste(text));
        h.run_steps(3);
    };

    // Skip: the clashing file is untouched, the new one still arrives.
    paste(&mut h);
    assert!(matches!(h.state().ui.dialog, Some(Dialog::Conflict { .. })));
    h.get_by_label("Skip").click();
    h.run_steps(1);
    wait_for_jobs(&mut h);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
        "hello"
    );
    assert!(dir.path().join("fresh.txt").exists());

    // Keep both: "notes (2).txt".
    std::fs::remove_file(dir.path().join("fresh.txt")).unwrap();
    h.run_steps(2);
    paste(&mut h);
    h.get_by_label("Keep both").click();
    h.run_steps(1);
    wait_for_jobs(&mut h);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("notes (2).txt")).unwrap(),
        "incoming"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("notes.txt")).unwrap(),
        "hello"
    );

    // Cancel does nothing at all.
    paste(&mut h);
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert!(h.state().ui.dialog.is_none());
    assert!(h.state().ui.jobs.is_empty());
}

#[test]
fn tabs_keep_their_own_folder_selection_and_history() {
    let dir = picture_folder();
    let sub = dir.path().join("Sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("inner.txt"), "x").unwrap();
    let mut h = harness(dir.path());

    h.get_by_label("IMG_3.jpg").click();
    h.run_steps(2);

    // Ctrl+T opens a second tab on the same folder; navigate it into Sub.
    h.key_press_modifiers(Modifiers::COMMAND, Key::T);
    h.run_steps(3);
    assert_eq!(h.state().ui.tabs.len(), 2);
    assert_eq!(h.state().ui.active_tab, 1);
    assert!(h.state().ui.selection.is_empty(), "new tab starts fresh");
    h.get_by_label("Sub").click();
    h.step();
    h.get_by_label("Sub").click();
    h.run_steps(3);
    assert_eq!(h.state().fs.get_current_path(), sub);

    // Ctrl+Tab back: first tab still on its folder with its selection.
    h.key_press_modifiers(Modifiers::COMMAND, Key::Tab);
    h.run_steps(3);
    assert_eq!(h.state().ui.active_tab, 0);
    assert_eq!(h.state().fs.get_current_path(), dir.path());
    assert!(h
        .state()
        .ui
        .selection
        .contains(&dir.path().join("IMG_3.jpg")));
    assert_eq!(h.state().ui.list.items.len(), 7);

    // ...and the second tab kept its own Back history.
    h.key_press_modifiers(Modifiers::COMMAND, Key::Tab);
    h.run_steps(3);
    assert_eq!(h.state().fs.get_current_path(), sub);
    assert!(h.state().fs.can_go_back());

    // Ctrl+W closes it; the remaining tab becomes active.
    h.key_press_modifiers(Modifiers::COMMAND, Key::W);
    h.run_steps(3);
    assert_eq!(h.state().ui.tabs.len(), 1);
    assert_eq!(h.state().fs.get_current_path(), dir.path());
    // The last tab can't be closed.
    h.key_press_modifiers(Modifiers::COMMAND, Key::W);
    h.run_steps(2);
    assert_eq!(h.state().ui.tabs.len(), 1);
}

#[test]
fn middle_click_opens_folder_in_background_tab_and_tabs_are_restored() {
    let dir = picture_folder();
    let sub = dir.path().join("Sub");
    std::fs::create_dir(&sub).unwrap();
    let mut h = harness(dir.path());

    let pos = h.get_by_label("Sub").rect().center();
    h.event(Event::PointerMoved(pos));
    h.step();
    for pressed in [true, false] {
        h.event(Event::PointerButton {
            pos,
            button: PointerButton::Middle,
            pressed,
            modifiers: Modifiers::NONE,
        });
        h.step();
    }
    h.run_steps(2);
    assert_eq!(h.state().ui.tabs.len(), 2);
    assert_eq!(
        h.state().ui.active_tab,
        0,
        "middle-click opens in the background"
    );
    assert_eq!(h.state().fs.get_current_path(), dir.path());

    // Saved settings record both tabs, and a new UI restores them.
    let saved = h.state().ui.settings(&h.state().fs);
    assert_eq!(saved.open_tabs, [dir.path().to_path_buf(), sub.clone()]);
    let cache = tempfile::tempdir().unwrap();
    let fs = FileSystem::new(dir.path().to_path_buf(), false);
    let restored = NinimmaUI::with_thumbnails(
        saved,
        &fs,
        ThumbnailCache::with_cache_dir(cache.path().into()),
    );
    assert_eq!(restored.tabs.len(), 2);
    assert_eq!(restored.tab_path(1, &fs), sub);
}

#[test]
fn cancelling_the_conflict_prompt_keeps_the_cut() {
    let dir = picture_folder();
    let sub = dir.path().join("Sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("notes.txt"), "other").unwrap();
    let mut h = harness(dir.path());

    // Cut notes.txt, go into Sub (which has its own notes.txt) and paste.
    h.get_by_label("notes.txt").click();
    h.run_steps(2);
    h.event(Event::Cut);
    h.run_steps(2);
    h.state_mut().fs.set_path(sub.clone());
    h.run_steps(3);
    h.key_press_modifiers(Modifiers::COMMAND, Key::V);
    h.run_steps(3);
    assert!(matches!(h.state().ui.dialog, Some(Dialog::Conflict { .. })));
    h.get_by_label("Cancel").click();
    h.run_steps(3);

    assert!(dir.path().join("notes.txt").exists(), "nothing moved");
    assert!(
        matches!(&h.state().ui.clipboard, Some((_, PasteMode::Move))),
        "the cut is still there to paste elsewhere"
    );
}

#[test]
fn hidden_files_toggle_applies_to_every_tab() {
    let dir = picture_folder();
    std::fs::write(dir.path().join(".secret"), "x").unwrap();
    let mut h = harness(dir.path());
    h.key_press_modifiers(Modifiers::COMMAND, Key::T);
    h.run_steps(3);
    h.key_press_modifiers(Modifiers::COMMAND, Key::H);
    h.run_steps(3);
    h.key_press_modifiers(Modifiers::COMMAND, Key::Tab);
    h.run_steps(3);
    assert!(
        h.state().fs.is_showing_hidden(),
        "the other tab follows the setting"
    );
    assert!(h.state().ui.list.items.iter().any(|i| i.name == ".secret"));
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn typing_a_share_address_asks_to_sign_in_then_opens_it() {
    let dir = picture_folder();
    let share = tempfile::tempdir().unwrap();
    std::fs::write(share.path().join("on-the-nas.txt"), "x").unwrap();
    let tools = tempfile::tempdir().unwrap();
    let gio = crate::network::tests::fake_gio_at(tools.path(), share.path());
    *crate::network::TEST_GIO.lock().unwrap() = Some(gio.to_string_lossy().to_string());
    let mut h = harness(dir.path());

    // Type the address into the address bar.
    h.key_press_modifiers(Modifiers::COMMAND, Key::L);
    h.run_steps(2);
    h.event(Event::Text("smb://nas/photos".into()));
    h.key_press(Key::Enter);
    let wait = |h: &mut Harness<'static, App>| {
        for _ in 0..300 {
            h.step();
            if h.state().ui.connecting.is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        h.run_steps(2);
    };
    wait(&mut h);
    // The server wants a password, so the dialog asks for one.
    match &mut h.state_mut().ui.dialog {
        Some(Dialog::Connect {
            needs_credentials: true,
            user,
            password,
            ..
        }) => {
            *user = "tom".into();
            *password = "secret".into();
        }
        other => panic!("expected sign-in prompt, got {}", other.is_some()),
    }
    h.get_by_label("Connect").click();
    h.run_steps(1);
    wait(&mut h);

    *crate::network::TEST_GIO.lock().unwrap() = None;
    assert!(h.state().ui.dialog.is_none(), "dialog closes on success");
    assert_eq!(h.state().fs.get_current_path(), share.path());
    assert_eq!(h.state().ui.settings.recent_servers, ["smb://nas/photos"]);
    assert!(h
        .state()
        .ui
        .list
        .items
        .iter()
        .any(|i| i.name == "on-the-nas.txt"));
}

#[test]
fn network_and_cloud_places_appear_in_the_sidebar() {
    let dir = picture_folder();
    let share = tempfile::tempdir().unwrap();
    let mut h = harness(dir.path());
    *h.state().ui.places.lock().unwrap() = vec![crate::network::Place {
        label: "photos on nas".into(),
        path: share.path().to_path_buf(),
        kind: crate::network::PlaceKind::Network,
        unmount: Some(crate::network::Unmount::Gio),
        note: None,
    }];
    h.run_steps(2);
    h.get_by_label("photos on nas").click();
    h.run_steps(3);
    assert_eq!(h.state().fs.get_current_path(), share.path());

    h.get_by_label("photos on nas")
        .click_button(PointerButton::Secondary);
    h.run_steps(3);
    assert!(
        h.query_by_label("⏏ Disconnect").is_some(),
        "disconnect offered for user mounts"
    );
}

#[test]
fn streaming_cloud_folders_skip_thumbnails_until_enabled() {
    let dir = picture_folder();
    let mut h = harness(dir.path());
    *h.state().ui.places.lock().unwrap() = vec![crate::network::Place {
        label: "Drive".into(),
        path: dir.path().to_path_buf(),
        kind: crate::network::PlaceKind::CloudStream,
        unmount: None,
        note: None,
    }];
    // Leave and come back so nothing is cached from the first visit.
    h.state_mut().ui.thumbnails.cancel_pending();
    h.run_steps(5);
    let img = dir.path().join("IMG_1.jpg");
    h.state_mut().ui.thumbnails.invalidate(&img);
    h.run_steps(5);
    assert!(h.state().ui.folder_is_cloud);
    assert!(
        !h.state().ui.thumbnails.was_requested(&img),
        "no download just to draw a thumbnail"
    );

    h.get_by_label("☁ Cloud thumbnails: off").click();
    h.run_steps(3);
    assert!(h.state().ui.thumbnails.was_requested(&img));
}

#[test]
fn slow_folders_show_loading_and_keep_the_window_responsive() {
    let dir = picture_folder();
    let slow = dir.path().join(crate::file_system::SLOW_TEST_FOLDER);
    std::fs::create_dir(&slow).unwrap();
    std::fs::write(slow.join("late.txt"), "x").unwrap();
    let mut h = harness(dir.path());

    let started = std::time::Instant::now();
    h.state_mut().fs.set_path(slow.clone());
    h.run_steps(2);
    // The folder takes 1.5 s to read; well under that proves the window didn't wait.
    assert!(started.elapsed() < Duration::from_millis(1000));
    assert!(
        h.query_all_by_label("Loading…").count() >= 1,
        "spinner while it loads"
    );
    for _ in 0..500 {
        h.step();
        if !h.state().fs.is_loading() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    h.run_steps(2);
    assert!(h.state().ui.list.items.iter().any(|i| i.name == "late.txt"));
}

fn open_menu(h: &mut Harness<'static, App>, path: &[&str]) {
    for (i, label) in path.iter().enumerate() {
        // Menu titles are exact; items' accessible names also include their
        // shortcut or submenu arrow.
        let node = if i == 0 {
            h.get_by_label(label)
        } else {
            h.get_by_label_contains(label)
        };
        if i + 1 < path.len() && i > 0 {
            node.hover(); // submenus open on hover
        } else {
            node.click();
        }
        h.run_steps(3);
    }
}

#[test]
fn file_menu_creates_text_documents_and_undo_removes_them() {
    let dir = picture_folder();
    let mut h = harness(dir.path());
    open_menu(&mut h, &["File", "New Document", "📃 Text Document"]);
    let created = dir.path().join("New Text Document.txt");
    assert!(created.exists());
    assert!(
        matches!(h.state().ui.dialog, Some(Dialog::Rename { .. })),
        "straight into renaming"
    );
    h.key_press(Key::Escape);
    h.run_steps(3);
    h.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    h.run_steps(1);
    wait_for_jobs(&mut h);
    assert!(!created.exists(), "undo removes the new (still empty) file");
}

#[test]
fn templates_appear_in_new_document_menus() {
    let dir = picture_folder();
    let templates = tempfile::tempdir().unwrap();
    std::fs::write(templates.path().join("Letter.odt"), "letter template").unwrap();
    let mut h = harness(dir.path());
    h.state_mut().ui.templates = crate::file_ops::list_templates(templates.path());

    // From the right-click menu on empty space.
    let p = Pos2::new(900.0, 800.0);
    h.event(Event::PointerMoved(p));
    h.step();
    for pressed in [true, false] {
        h.event(Event::PointerButton {
            pos: p,
            button: PointerButton::Secondary,
            pressed,
            modifiers: Modifiers::NONE,
        });
        h.step();
    }
    h.run_steps(2);
    h.get_by_label_contains("📄 New Document").hover();
    h.run_steps(3);
    h.get_by_label_contains("Letter").click();
    h.run_steps(3);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("Letter.odt")).unwrap(),
        "letter template"
    );
}

#[test]
fn ctrl_d_bookmarks_the_folder_and_it_shows_in_the_sidebar() {
    let dir = picture_folder();
    let mut h = harness(dir.path());
    h.key_press_modifiers(Modifiers::COMMAND, Key::D);
    h.run_steps(3);
    let file = h.state().ui.bookmarks_file.clone().unwrap();
    let saved = crate::bookmarks::load(&file);
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].path, dir.path());
    assert!(h.query_by_label("🔖 Bookmarks").is_some());

    h.key_press_modifiers(Modifiers::COMMAND, Key::D);
    h.run_steps(3);
    assert!(
        crate::bookmarks::load(&file).is_empty(),
        "Ctrl+D again removes it"
    );
}

#[test]
#[cfg(any(windows, target_os = "linux"))]
fn disk_dialog_defaults_to_readonly_and_can_request_writes() {
    let dir = picture_folder();
    let mut h = harness(dir.path());
    open_menu(&mut h, &["File", "Disk images & BitLocker…"]);
    assert!(matches!(
        h.state().ui.dialog,
        Some(Dialog::Disks {
            operation: DiskOperation::Mount,
            read_only: true,
            ..
        })
    ));
    h.get_by_label("Read-only").click();
    h.run_steps(3);
    assert!(matches!(
        h.state().ui.dialog,
        Some(Dialog::Disks {
            read_only: false,
            ..
        })
    ));
    // Never start a privileged operation from a test.
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert!(h.state().ui.dialog.is_none());
    assert!(h.state().ui.disk_launch.is_none());
}

#[test]
fn invert_selection_and_help() {
    let dir = picture_folder();
    let mut h = harness(dir.path());
    h.get_by_label("IMG_1.jpg").click();
    h.run_steps(2);
    open_menu(&mut h, &["Edit", "Invert Selection"]);
    assert_eq!(h.state().ui.selection.len(), 5);
    assert!(!h
        .state()
        .ui
        .selection
        .contains(&dir.path().join("IMG_1.jpg")));

    h.key_press(Key::F1);
    h.run_steps(3);
    assert!(matches!(h.state().ui.dialog, Some(Dialog::Shortcuts)));
}

// ----------------------------------------------------------------------
// Search index
// ----------------------------------------------------------------------

/// Steps the UI until `done` holds (the index works in the background).
fn step_until(h: &mut Harness<'static, App>, what: &str, done: impl Fn(&App) -> bool) {
    for _ in 0..2000 {
        h.step();
        if done(h.state()) {
            h.run_steps(2);
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for {what}");
}

/// Turns indexing on for `root`, with a private index folder and an
/// in-memory key store, and waits until `files` items are indexed.
fn enable_index(h: &mut Harness<'static, App>, root: &Path, files: u64) -> tempfile::TempDir {
    let index_dir = tempfile::tempdir().unwrap();
    let store = Arc::new(crate::security::key_store::MemoryKeyStore::default());
    let app = h.state_mut();
    app.ui.index.test_setup = Some((index_dir.path().join("ninimma/index"), store));
    app.ui.settings.index = IndexSettings {
        enabled: true,
        roots: vec![root.to_path_buf()],
        pause_on_battery: false,
        ..Default::default()
    };
    step_until(h, "the index to be built", |app| {
        app.ui.index.status.as_ref().is_some_and(|s| {
            s.state == crate::index::types::IndexState::Ready && s.indexed_files == files
        })
    });
    index_dir
}

fn pick_scope(h: &mut Harness<'static, App>, label: &str) {
    h.get_by_label("🔍⏷").click();
    h.run_steps(2);
    h.get_by_label(label).click();
    h.run_steps(2);
}

fn listed(app: &App) -> Vec<String> {
    let mut names: Vec<String> = app.ui.list.items.iter().map(|i| i.name.clone()).collect();
    names.sort();
    names
}

#[test]
fn searching_subfolders_and_indexed_locations_uses_the_index() {
    let root = tempfile::tempdir().unwrap();
    for f in [
        "Trips/2025/beach.jpg",
        "Trips/notes.txt",
        "Work/beach-house.pdf",
    ] {
        let p = root.path().join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "").unwrap();
    }
    let trips = root.path().join("Trips");
    let mut h = harness(&trips);
    // Trips, 2025, beach.jpg, notes.txt, Work, beach-house.pdf
    let _index = enable_index(&mut h, root.path(), 6);

    // This folder only: beach.jpg is a level down, so nothing matches.
    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    h.run_steps(2);
    h.event(Event::Text("beach".into()));
    h.run_steps(3);
    assert!(listed(h.state()).is_empty());

    pick_scope(&mut h, "This folder and subfolders");
    step_until(&mut h, "subfolder results", |app| {
        !app.ui.list.items.is_empty()
    });
    assert_eq!(listed(h.state()), ["beach.jpg"]);

    pick_scope(&mut h, "All indexed locations");
    step_until(&mut h, "results everywhere", |app| {
        app.ui.list.items.len() == 2
    });
    assert_eq!(listed(h.state()), ["beach-house.pdf", "beach.jpg"]);

    // Opening a folder from the results goes there and clears the search.
    h.state_mut().ui.search_text = "2025".into();
    // The folder ranks first; with full text its contents match too.
    step_until(&mut h, "the folder result", |app| {
        app.ui.list.items.first().is_some_and(|i| i.name == "2025")
    });
    h.get_by_label("2025").click();
    h.run_steps(1);
    h.get_by_label("2025").click();
    h.run_steps(4);
    assert_eq!(h.state().fs.get_current_path(), trips.join("2025"));
    assert!(h.state().ui.search_text.is_empty());
}

#[test]
fn index_scopes_explain_how_to_turn_indexing_on() {
    let dir = picture_folder();
    let mut h = harness(dir.path());
    pick_scope(&mut h, "This folder and subfolders");
    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    h.run_steps(2);
    h.event(Event::Text("IMG".into()));
    h.run_steps(3);
    assert!(h.state().ui.list.items.is_empty());
    assert!(h
        .query_by_label("Turn on search indexing (Edit > Search Indexing) to search subfolders.")
        .is_some());
    // The plain folder filter still works without any index.
    pick_scope(&mut h, "This folder");
    h.run_steps(2);
    assert_eq!(h.state().ui.list.items.len(), 5);
}

#[test]
fn indexing_is_saved_as_on_only_once_the_index_exists() {
    let dir = picture_folder();
    let mut h = harness(dir.path());
    let index_dir = tempfile::tempdir().unwrap();
    let store = Arc::new(crate::security::key_store::MemoryKeyStore::default());
    h.state_mut().ui.index.test_setup = Some((index_dir.path().join("ninimma/index"), store));
    h.state_mut().ui.settings.index.pause_on_battery = false;
    // Chosen first: with no folders, turning it on would pick this
    // machine's real Documents, Pictures etc.
    h.state_mut().ui.settings.index.roots = vec![dir.path().to_path_buf()];

    open_menu(&mut h, &["Edit", "Search Indexing…"]);
    assert!(!h.state().ui.settings.index.enabled);
    h.get_by_label("Index files for searching").click();
    step_until(&mut h, "indexing to be on", |app| {
        app.ui.settings.index.enabled
    });
    assert!(index_dir.path().join("ninimma/index/metadata.db").exists());
    assert_eq!(h.state().ui.settings.index.roots, [dir.path()]);
    step_until(&mut h, "the scan", |app| {
        app.ui
            .index
            .status
            .as_ref()
            .is_some_and(|s| s.indexed_files == 6)
    });
    assert!(h.query_by_label("Delete index and key…").is_some());

    // Deleting turns indexing off and removes the index.
    h.get_by_label("Delete index and key…").click();
    h.run_steps(2);
    h.get_by_label("Delete").click();
    step_until(&mut h, "deletion", |app| !app.ui.settings.index.enabled);
    assert!(!index_dir.path().join("ninimma/index/metadata.db").exists());
    assert!(
        dir.path().join("IMG_1.jpg").exists(),
        "the user's files are untouched"
    );
}

#[test]
fn settings_saved_by_the_previous_release_still_load() {
    // As 0.2.0 saved them (RON, via eframe), with no search index fields.
    let old = r#"(
        show_hidden: true,
        show_nav_pane: true,
        show_preview_pane: false,
        last_path: Some("/home/a/Pictures"),
        open_tabs: ["/home/a/Pictures"],
        active_tab: 0,
        recent_servers: ["smb://nas/photos"],
        cloud_thumbnails: false,
        bookmarks: [],
    )"#;
    let settings: Settings = ron::from_str(old).unwrap();
    assert!(settings.show_hidden);
    assert!(!settings.show_preview_pane);
    assert!(!settings.index.enabled, "indexing stays off");
    assert!(settings.index.pause_on_battery);
    assert_eq!(settings.search_scope, SearchScope::ThisFolder);
}

#[test]
fn a_search_typed_before_the_index_opens_runs_once_it_does() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("sub")).unwrap();
    std::fs::write(root.path().join("sub/early-bird.txt"), "").unwrap();
    let mut h = harness(root.path());
    h.state_mut().ui.settings.search_scope = SearchScope::Subfolders;
    h.state_mut().ui.search_text = "early".into();
    h.run_steps(2);
    assert!(h.state().ui.list.items.is_empty());
    let _index = enable_index(&mut h, root.path(), 2);
    step_until(&mut h, "the early search", |app| {
        listed(app) == ["early-bird.txt"]
    });
}

#[test]
fn smart_search_is_explained_before_it_turns_on() {
    if !crate::embedding::is_built_in() {
        return;
    }
    let dir = picture_folder();
    let mut h = harness(dir.path());
    let index_dir = tempfile::tempdir().unwrap();
    let store = Arc::new(crate::security::key_store::MemoryKeyStore::default());
    h.state_mut().ui.index.test_setup = Some((index_dir.path().join("ninimma/index"), store));
    h.state_mut().ui.settings.index.roots = vec![dir.path().to_path_buf()];
    open_menu(&mut h, &["Edit", "Search Indexing…"]);
    h.get_by_label_contains("Smart: also meaning-based").click();
    h.run_steps(2);
    // Not on yet: the explanation is shown first.
    assert_ne!(
        h.state().ui.settings.index.mode,
        crate::index::config::IndexMode::Smart
    );
    assert!(h
        .query_by_label_contains("runs entirely on this computer")
        .is_some());
    h.get_by_label("Turn on smart search").click();
    h.run_steps(2);
    assert_eq!(
        h.state().ui.settings.index.mode,
        crate::index::config::IndexMode::Smart
    );
    assert!(h.state().ui.settings.index.smart_acknowledged);
    // With no models installed, the dialog offers to get them: the text
    // model, and the picture model (searching what photos show is on).
    if !crate::models::is_installed(&crate::models::EMBEDDING_MODEL) {
        assert_eq!(h.query_all_by_label_contains("Download (").count(), 2);
        assert!(h.query_by_label_contains("by what they show").is_some());
    }
}

#[test]
fn ocr_is_offered_with_full_text_and_explains_itself() {
    if !crate::embedding::is_built_in() {
        return;
    }
    let dir = picture_folder();
    let mut h = harness(dir.path());
    let index_dir = tempfile::tempdir().unwrap();
    let store = Arc::new(crate::security::key_store::MemoryKeyStore::default());
    h.state_mut().ui.index.test_setup = Some((index_dir.path().join("ninimma/index"), store));
    h.state_mut().ui.settings.index.roots = vec![dir.path().to_path_buf()];
    open_menu(&mut h, &["Edit", "Search Indexing…"]);
    h.get_by_label_contains("Read text in photos of documents")
        .click();
    h.run_steps(2);
    assert!(h.state().ui.settings.index.ocr);
    assert!(h
        .query_by_label_contains("with IBM's granite-docling model")
        .is_some());
    assert!(
        h.query_by_label_contains("500 MB").is_some(),
        "the memory cost is stated"
    );
    if !crate::models::is_installed(&crate::models::OCR_MODEL) {
        assert!(h.query_by_label_contains("Download (").is_some());
    }
    // Names only: OCR can't be chosen.
    h.get_by_label_contains("Basic: names").click();
    h.run_steps(2);
    assert!(h
        .query_by_label_contains("with IBM's granite-docling model")
        .is_none());
}

/// Clicks with the real pointer (not an accessibility action), so whatever
/// is on top at that spot gets the click, as in the app.
fn mouse_click(h: &mut Harness<'static, App>, pos: Pos2) {
    h.event(Event::PointerMoved(pos));
    h.step();
    for pressed in [true, false] {
        h.event(Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
        h.step();
    }
    h.run_steps(3);
}

#[test]
fn clicking_a_folder_in_the_address_bar_goes_there() {
    let root = tempfile::tempdir().unwrap();
    let deep = root.path().join("crumb_alpha/crumb_beta/crumb_gamma");
    std::fs::create_dir_all(&deep).unwrap();
    let mut h = harness(&deep);
    // The address bar is the topmost element with that name (the folder
    // may also appear in the navigation pane).
    let crumb = |h: &Harness<'static, App>, name: &str| {
        h.get_all_by_label(name)
            .map(|n| n.rect())
            .min_by(|a, b| a.top().total_cmp(&b.top()))
            .unwrap_or_else(|| panic!("{name} isn't shown"))
            .center()
    };
    let pos = crumb(&h, "crumb_alpha");
    mouse_click(&mut h, pos);
    assert_eq!(
        h.state().fs.get_current_path(),
        root.path().join("crumb_alpha")
    );
    assert!(
        !h.state().ui.editing_address,
        "clicking a folder name doesn't start typing"
    );
    // The empty part of the bar, right of the last folder, still switches
    // to typing a path.
    let bar = h
        .ctx
        .data(|d| d.get_temp::<Rect>(Id::unique("address_bar_bg")))
        .expect("the address bar is shown");
    let empty = Pos2::new(bar.right() - 20.0, bar.center().y);
    mouse_click(&mut h, empty);
    assert!(h.state().ui.editing_address);
    assert_eq!(
        h.state().fs.get_current_path(),
        root.path().join("crumb_alpha")
    );
}

/// The node with this label furthest right (the main area, not the sidebar).
fn rightmost(h: &Harness<'static, App>, label: &str) -> Rect {
    h.get_all_by_label(label)
        .map(|n| n.rect())
        .max_by(|a, b| a.left().total_cmp(&b.left()))
        .unwrap_or_else(|| panic!("{label} isn't shown"))
}

#[test]
fn this_pc_shows_every_drive_and_opens_one() {
    let dir = picture_folder();
    let disk = tempfile::tempdir().unwrap();
    let mut h = harness(dir.path());
    *h.state().ui.drives.lock().unwrap() = vec![DriveInfo {
        path: disk.path().to_path_buf(),
        label: "Data disk".into(),
        drive_type: DriveType::Fixed,
        total_space: Some(100 << 30),
        free_space: Some(40 << 30),
    }];
    h.run_steps(2);
    h.get_by_label("💻 This PC").click();
    h.run_steps(3);
    assert_eq!(h.state().ui.special, Some(Special::ThisPc));
    // The address bar says where we are, and the tile shows the free space.
    assert_eq!(h.get_all_by_label("💻 This PC").count(), 2);
    let free = format!(
        "{} free of {}",
        format_bytes(40 << 30),
        format_bytes(100 << 30)
    );
    assert!(h.query_by_label(&free).is_some(), "{free}");
    // Clicking the drive's tile opens it.
    let tile = rightmost(&h, "Data disk");
    mouse_click(&mut h, tile.center());
    assert_eq!(h.state().fs.get_current_path(), disk.path());
    assert_eq!(h.state().ui.special, None);
}

#[test]
fn up_from_the_top_goes_to_this_pc_and_back_returns() {
    let mut h = harness(Path::new("/"));
    h.key_press_modifiers(Modifiers::ALT, Key::ArrowUp);
    h.run_steps(2);
    assert_eq!(h.state().ui.special, Some(Special::ThisPc));
    // Up again stays; Back returns to the folder.
    h.key_press_modifiers(Modifiers::ALT, Key::ArrowUp);
    h.run_steps(2);
    assert_eq!(h.state().ui.special, Some(Special::ThisPc));
    h.key_press_modifiers(Modifiers::ALT, Key::ArrowLeft);
    h.run_steps(2);
    assert_eq!(h.state().ui.special, None);
    assert_eq!(h.state().fs.get_current_path(), Path::new("/"));
}

#[test]
fn recent_lists_opened_files_and_visited_folders_newest_first() {
    let dir = picture_folder();
    std::fs::create_dir(dir.path().join("Trips")).unwrap();
    let mut h = harness(dir.path());
    // Open a file, then go into a folder.
    for _ in 0..2 {
        h.get_by_label("notes.txt").click();
        h.step();
    }
    h.run_steps(2);
    assert_eq!(h.state().ui.opened, [dir.path().join("notes.txt")]);
    for _ in 0..2 {
        h.get_by_label("Trips").click();
        h.step();
    }
    h.run_steps(3);
    assert_eq!(h.state().fs.get_current_path(), dir.path().join("Trips"));

    h.get_by_label("Recent").click();
    h.run_steps(3);
    assert_eq!(h.state().ui.special, Some(Special::Recent));
    let names: Vec<String> = h
        .state()
        .ui
        .list
        .items
        .iter()
        .map(|i| i.name.clone())
        .collect();
    assert_eq!(
        names[..2],
        ["Trips", "notes.txt"],
        "newest first: {names:?}"
    );
    // Opening a folder from Recent leaves it.
    let trips = rightmost(&h, "Trips");
    mouse_click(&mut h, trips.center());
    mouse_click(&mut h, trips.center());
    assert_eq!(h.state().ui.special, None);
    assert_eq!(h.state().fs.get_current_path(), dir.path().join("Trips"));
}

#[test]
fn turning_recent_off_forgets_it_and_hides_it() {
    let dir = picture_folder();
    std::fs::create_dir(dir.path().join("Trips")).unwrap();
    let mut h = harness(dir.path());
    for _ in 0..2 {
        h.get_by_label("Trips").click();
        h.step();
    }
    h.run_steps(3);
    assert!(!h.state().ui.settings.recent.is_empty());
    h.get_by_label("Recent").click();
    h.run_steps(2);
    h.state_mut().ui.settings.show_recent = true;
    let ctx = h.ctx.clone();
    {
        let state = h.state_mut();
        state.ui.apply(Action::ToggleRecent, &ctx, &mut state.fs);
    }
    h.run_steps(3);
    assert!(
        h.state().ui.settings.recent.is_empty(),
        "off means not kept"
    );
    assert_eq!(h.state().ui.special, None, "the Recent view closes");
    assert!(
        h.query_by_label("Recent").is_none(),
        "and leaves the sidebar"
    );
    // While off, nothing is recorded.
    {
        let state = h.state_mut();
        state.ui.apply(
            Action::Navigate(dir.path().to_path_buf()),
            &ctx,
            &mut state.fs,
        );
    }
    h.run_steps(3);
    {
        let state = h.state_mut();
        state.ui.apply(
            Action::Navigate(dir.path().join("Trips")),
            &ctx,
            &mut state.fs,
        );
    }
    h.run_steps(3);
    assert_eq!(h.state().fs.get_current_path(), dir.path().join("Trips"));
    assert!(h.state().ui.settings.recent.is_empty());
}

#[test]
fn this_pc_tiles_line_up_in_rows_and_wrap_inside_the_window() {
    // Five drives don't fit on one row of a 1400-point window (the middle,
    // between the side panes, has room for three): they wrap,
    // every tile is inside the window, and each row's tiles share a top
    // (laid out as they were drawn, tiles stepped down and ran off).
    let dir = picture_folder();
    let width = 1400.0;
    let mut h = harness_sized(dir.path(), Vec2::new(width, 700.0));
    *h.state().ui.drives.lock().unwrap() = (1..=5)
        .map(|i| DriveInfo {
            path: dir.path().join(format!("drive-{i}")),
            label: format!("Drive {i}"),
            drive_type: if i == 5 {
                DriveType::Removable
            } else {
                DriveType::Fixed
            },
            total_space: Some(100 << 30),
            free_space: Some((i as u64 * 10) << 30),
        })
        .collect();
    h.state_mut().ui.special = Some(Special::ThisPc);
    h.run_steps(4);
    let tiles: Vec<Rect> = (1..=5)
        .map(|i| rightmost(&h, &format!("Drive {i}")))
        .collect();
    for (i, t) in tiles.iter().enumerate() {
        assert!(
            t.left() - 60.0 + PLACE_TILE_MIN.x <= width + 1.0,
            "tile {} runs off the right edge: {t:?}",
            i + 1
        );
    }
    let mut rows: Vec<f32> = Vec::new();
    for t in &tiles {
        if !rows.iter().any(|top| (top - t.top()).abs() < 1.0) {
            rows.push(t.top());
        }
    }
    assert!(
        rows.len() >= 2,
        "five tiles wrap onto more than one row: {tiles:?}"
    );
    let first_row: Vec<&Rect> = tiles
        .iter()
        .filter(|t| (t.top() - rows[0]).abs() < 1.0)
        .collect();
    assert!(
        first_row.len() >= 2,
        "several tiles share the first row: {tiles:?}"
    );
}

#[test]
fn place_tiles_fill_the_row_responsively() {
    // As many as fit at the minimum width, stretched to fill the row.
    for available in [300.0, 520.0, 800.0, 1234.0] {
        let size = place_tile_size(available);
        let columns = ((available + PLACE_TILE_GAP) / (size.x + PLACE_TILE_GAP)).floor();
        let used = columns * size.x + (columns - 1.0) * PLACE_TILE_GAP;
        assert!(size.x >= PLACE_TILE_MIN.x, "{available}: {size:?}");
        assert!(used <= available + 0.5, "{available}: fits ({used})");
        assert!(
            available - used < 1.0,
            "{available}: fills the row ({used})"
        );
    }
    // Narrower than one tile: one column, as wide as there is.
    assert_eq!(place_tile_size(180.0).x, 180.0);
    // Very wide: capped, not stretched absurdly.
    assert!(place_tile_size(5000.0).x <= PLACE_TILE_MAX_WIDTH);
}

/// Opens the right-click menu at `pos` with the real pointer.
fn right_click(h: &mut Harness<'static, App>, pos: Pos2) {
    h.event(Event::PointerMoved(pos));
    h.step();
    for pressed in [true, false] {
        h.event(Event::PointerButton {
            pos,
            button: PointerButton::Secondary,
            pressed,
            modifiers: Modifiers::NONE,
        });
        h.step();
    }
    h.run_steps(2);
}

#[test]
fn sidebar_drives_open_in_a_new_tab_from_their_menu() {
    let dir = picture_folder();
    let disk = tempfile::tempdir().unwrap();
    let mut h = harness(dir.path());
    *h.state().ui.drives.lock().unwrap() = vec![DriveInfo {
        path: disk.path().to_path_buf(),
        label: "Data disk".into(),
        drive_type: DriveType::Fixed,
        total_space: Some(100 << 30),
        free_space: Some(40 << 30),
    }];
    h.run_steps(2);
    let drive = h.get_by_label("Data disk").rect();
    right_click(&mut h, drive.center());
    h.get_by_label("🗂 Open in new tab").click();
    h.run_steps(3);
    assert_eq!(h.state().ui.tabs.len(), 2);
    assert_eq!(
        h.state().fs.get_current_path(),
        disk.path(),
        "the new tab is shown"
    );
}

#[test]
fn open_file_location_from_recent_selects_the_file_in_its_folder() {
    let dir = picture_folder();
    let mut h = harness(dir.path());
    // Opened before, so it's in Recent.
    for _ in 0..2 {
        h.get_by_label("notes.txt").click();
        h.step();
    }
    h.run_steps(2);
    h.state_mut().ui.special = Some(Special::Recent);
    let ctx = h.ctx.clone();
    {
        let state = h.state_mut();
        state
            .ui
            .apply(Action::ShowSpecial(Special::Recent), &ctx, &mut state.fs);
    }
    h.run_steps(3);
    let notes = rightmost(&h, "notes.txt");
    right_click(&mut h, notes.center());
    h.get_by_label("📂 Open file location").click();
    h.run_steps(5);
    assert_eq!(h.state().ui.special, None);
    assert_eq!(h.state().fs.get_current_path(), dir.path());
    assert_eq!(
        h.state().ui.selected_paths(),
        [dir.path().join("notes.txt")],
        "the file is selected in its folder"
    );
}

#[test]
fn open_file_location_in_new_tab_from_a_search_result() {
    let root = tempfile::tempdir().unwrap();
    let photo = root.path().join("Trips/2025/beach.jpg");
    std::fs::create_dir_all(photo.parent().unwrap()).unwrap();
    std::fs::write(&photo, "").unwrap();
    let mut h = harness(root.path());
    // Trips, 2025, beach.jpg
    let _index = enable_index(&mut h, root.path(), 3);
    h.key_press_modifiers(Modifiers::COMMAND, Key::F);
    h.run_steps(2);
    h.event(Event::Text("beach".into()));
    h.run_steps(3);
    pick_scope(&mut h, "This folder and subfolders");
    step_until(&mut h, "subfolder results", |app| {
        !app.ui.list.items.is_empty()
    });
    let beach = rightmost(&h, "beach.jpg");
    right_click(&mut h, beach.center());
    h.get_by_label("🗂 Open file location in new tab").click();
    h.run_steps(5);
    assert_eq!(h.state().ui.tabs.len(), 2);
    assert_eq!(h.state().fs.get_current_path(), photo.parent().unwrap());
    assert!(
        h.state().ui.search_text.is_empty(),
        "the new tab isn't searching"
    );
    assert_eq!(
        h.state().ui.selected_paths(),
        [photo],
        "the file is selected in its folder"
    );
}

#[test]
fn alt_enter_shows_properties_of_the_selected_image() {
    let dir = picture_folder();
    let mut h = harness(dir.path());
    h.get_by_label("IMG_1.jpg").click();
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::ALT, Key::Enter);
    h.run_steps(3);
    assert!(matches!(
        h.state().ui.dialog,
        Some(Dialog::Properties { .. })
    ));
    assert!(h.query_by_label("IMG_1.jpg — Properties").is_some());
    assert!(h.query_by_label("40 × 20 pixels").is_some(), "dimensions");
    let kind = FileItem::new(dir.path().join("IMG_1.jpg"))
        .unwrap()
        .get_type_name();
    assert!(h.get_all_by_label(&kind).count() > 0, "type: {kind}");
    h.get_by_label("Close").click();
    h.run_steps(2);
    assert!(h.state().ui.dialog.is_none());
}

#[test]
fn properties_with_nothing_selected_counts_the_folder() {
    let dir = picture_folder();
    std::fs::create_dir(dir.path().join("Sub")).unwrap();
    std::fs::write(dir.path().join("Sub/inner.txt"), "12345").unwrap();
    let mut h = harness(dir.path());
    // Nothing selected: Properties is about the folder itself.
    h.key_press_modifiers(Modifiers::ALT, Key::Enter);
    h.run_steps(2);
    assert!(h
        .query_by_label(&format!("{} — Properties", display_name(dir.path())))
        .is_some());
    let want = "7 files, 1 folders";
    let deadline = Instant::now() + Duration::from_secs(10);
    while h.query_by_label(want).is_none() {
        assert!(Instant::now() < deadline, "folder never counted");
        h.run_steps(2);
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A small set of apps, as if installed, for Open with.
#[cfg(all(unix, not(target_os = "macos")))]
fn test_apps(root: &Path) -> crate::open_with::Catalog {
    let write = |p: PathBuf, text: &str| {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    let data = root.join("share");
    write(data.join("mime/globs2"), "50:text/plain:*.txt\n");
    write(data.join("applications/editor.desktop"), "[Desktop Entry]\nType=Application\nName=Text Editor\nExec=editor %F\nMimeType=text/plain;\n");
    write(data.join("applications/notepad.desktop"), "[Desktop Entry]\nType=Application\nName=Notepad Plus\nExec=notepad %F\nMimeType=text/plain;\n");
    write(
        data.join("applications/paint.desktop"),
        "[Desktop Entry]\nType=Application\nName=Paint\nExec=paint %F\nMimeType=image/png;\n",
    );
    write(
        root.join("cfg/mimeapps.list"),
        "[Default Applications]\ntext/plain=editor.desktop;\n",
    );
    crate::open_with::Catalog::load(&crate::open_with::Dirs {
        data: vec![data],
        config: vec![root.join("cfg")],
        desktops: Vec::new(),
    })
}

#[test]
#[cfg(all(unix, not(target_os = "macos")))]
fn open_with_lists_the_default_and_other_apps_and_launches_one() {
    let dir = picture_folder();
    let apps = tempfile::tempdir().unwrap();
    let mut h = harness(dir.path());
    let _ = h.state().ui.apps.set(test_apps(apps.path()));
    let notes = h.get_by_label("notes.txt").rect();
    right_click(&mut h, notes.center());
    h.get_by_label_contains("Open with").click();
    h.run_steps(3);
    assert!(h.query_by_label("Text Editor (default)").is_some());
    assert!(
        h.query_by_label("Paint").is_none(),
        "only apps for text files"
    );
    h.get_by_label("Notepad Plus").click();
    h.run_steps(2);
    assert_eq!(
        h.state().ui.launched,
        [(
            "notepad.desktop".to_string(),
            vec![dir.path().join("notes.txt")]
        )]
    );
}

#[test]
#[cfg(all(unix, not(target_os = "macos")))]
fn choosing_another_app_can_make_it_the_default() {
    let dir = picture_folder();
    let apps = tempfile::tempdir().unwrap();
    let mut h = harness(dir.path());
    let _ = h.state().ui.apps.set(test_apps(apps.path()));
    h.get_by_label("notes.txt").click();
    h.run_steps(2);
    let ctx = h.ctx.clone();
    {
        let state = h.state_mut();
        state.ui.apply(Action::ChooseApp, &ctx, &mut state.fs);
    }
    h.run_steps(3);
    assert!(h.query_by_label("Open notes.txt with").is_some());
    // Any app can be chosen, not only suggested ones.
    assert!(h.query_by_label("Paint").is_some());
    h.get_by_label("Always use this app for .txt files").click();
    h.run_steps(2);
    h.get_by_label("Notepad Plus").click();
    h.run_steps(3);
    assert!(h.state().ui.dialog.is_none());
    assert_eq!(h.state().ui.launched.len(), 1);
    assert_eq!(
        h.state().ui.defaults_set,
        [("notepad.desktop".to_string(), "text/plain".to_string())]
    );
}

/// The node with this label furthest left (the navigation pane).
fn leftmost(h: &Harness<'static, App>, label: &str) -> Rect {
    h.get_all_by_label(label)
        .map(|n| n.rect())
        .min_by(|a, b| a.left().total_cmp(&b.left()))
        .unwrap_or_else(|| panic!("{label} isn't shown"))
}

/// Steps until `label` is shown (background work), or fails.
fn wait_for_label(h: &mut Harness<'static, App>, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while h.query_all_by_label(label).next().is_none() {
        assert!(Instant::now() < deadline, "{label} never appeared");
        h.run_steps(2);
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn the_folder_tree_expands_drives_and_opens_subfolders() {
    let dir = picture_folder();
    let disk = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(disk.path().join("Alpha/Inner")).unwrap();
    std::fs::create_dir(disk.path().join("Beta")).unwrap();
    std::fs::write(disk.path().join("file.txt"), "").unwrap();
    let mut h = harness(dir.path());
    *h.state().ui.drives.lock().unwrap() = vec![DriveInfo {
        path: disk.path().to_path_buf(),
        label: "Data disk".into(),
        drive_type: DriveType::Fixed,
        total_space: Some(100 << 30),
        free_space: Some(40 << 30),
    }];
    h.run_steps(2);
    h.get_by_label("Expand Data disk").click();
    wait_for_label(&mut h, "Alpha");
    assert!(h.query_by_label("Beta").is_some());
    assert!(h.query_by_label("file.txt").is_none(), "folders only");
    assert_eq!(
        h.state().fs.get_current_path(),
        dir.path(),
        "the arrow doesn't open it"
    );
    // A second level.
    h.get_by_label("Expand Alpha").click();
    wait_for_label(&mut h, "Inner");
    // Clicking a folder in the tree opens it.
    let inner = leftmost(&h, "Inner");
    mouse_click(&mut h, inner.center());
    assert_eq!(
        h.state().fs.get_current_path(),
        disk.path().join("Alpha/Inner")
    );
    // Collapsing hides the subfolders.
    h.get_by_label("Collapse Data disk").click();
    h.run_steps(2);
    assert!(h.query_by_label("Beta").is_none());
}

#[test]
fn pinning_a_folder_adds_it_to_quick_access_until_unpinned() {
    let dir = picture_folder();
    std::fs::create_dir(dir.path().join("Projects")).unwrap();
    let mut h = harness(dir.path());
    let folder = h.get_by_label("Projects").rect();
    right_click(&mut h, folder.center());
    // (Right-clicking selects it, so the preview pane names it too.)
    let shown = h.get_all_by_label("Projects").count();
    h.get_by_label("📌 Pin to Quick Access").click();
    h.run_steps(2);
    assert_eq!(h.state().ui.settings.pinned, [dir.path().join("Projects")]);
    // In the navigation pane now, as well.
    assert_eq!(h.get_all_by_label("Projects").count(), shown + 1);
    let pinned = leftmost(&h, "Projects");
    right_click(&mut h, pinned.center());
    h.get_by_label("📌 Unpin from Quick Access").click();
    h.run_steps(2);
    assert!(h.state().ui.settings.pinned.is_empty());
    assert_eq!(h.get_all_by_label("Projects").count(), shown);
}

#[test]
fn the_tree_arrows_are_glyphs_the_fonts_have() {
    // A missing glyph draws as an empty box.
    let dir = picture_folder();
    let h = harness(dir.path());
    for glyph in [TREE_COLLAPSED, TREE_EXPANDED] {
        let has = h
            .ctx
            .fonts_mut(|f| f.has_glyphs(&FontId::proportional(FONT_SIZE_SMALL), glyph));
        assert!(has, "{glyph} would show as a box");
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn show_in_folder_from_a_browser_opens_the_folder_and_selects_the_file() {
    use crate::file_manager1::Request;
    let dir = picture_folder();
    let downloads = dir.path().join("Downloads");
    std::fs::create_dir(&downloads).unwrap();
    let file = downloads.join("report.pdf");
    std::fs::write(&file, "pdf").unwrap();
    let mut h = harness(dir.path());
    let (tx, rx) = std::sync::mpsc::channel();
    h.state_mut().ui.file_manager1 = Some(rx);

    tx.send(Request::Items(vec![file.clone()])).unwrap();
    h.run_steps(6);
    assert_eq!(h.state().fs.get_current_path(), downloads);
    assert!(h.state().ui.selection.contains(&file));
}

#[test]
fn recycle_bin_restores_and_deletes_for_good() {
    let dir = picture_folder();
    let kept = dir.path().join("notes.txt");
    let gone = dir.path().join("old.txt");
    std::fs::write(&gone, "old").unwrap();
    crate::trash_bin::imp::put(&kept, 200);
    crate::trash_bin::imp::put(&gone, 100);
    let mut h = harness(dir.path());

    let bin = if cfg!(windows) {
        "Recycle Bin"
    } else {
        "Trash"
    };
    h.get_by_label(bin).click();
    h.run_steps(3);
    h.get_by_label("notes.txt").click();
    h.run_steps(2);
    h.get_by_label("↩ Restore").click();
    h.run_steps(3);
    assert_eq!(
        std::fs::read_to_string(&kept).unwrap(),
        "hello",
        "back where it was"
    );

    h.get_by_label(&format!("🧹 Empty {bin}")).click();
    h.run_steps(3);
    h.get_by_label("Delete").click();
    h.run_steps(3);
    assert!(crate::trash_bin::list().unwrap().is_empty());
    assert!(!gone.exists());
}

#[test]
fn paste_is_enabled_for_files_copied_in_another_app() {
    let dir = picture_folder();
    let elsewhere = tempfile::tempdir().unwrap();
    let report = elsewhere.path().join("report.pdf");
    std::fs::write(&report, "pdf").unwrap();
    crate::system_clipboard::copied_elsewhere(&[report.as_path()]);
    let mut h = harness(dir.path());
    h.run_steps(2);
    h.get_by_label("📋 Paste").click();
    h.run_steps(1);
    wait_for_jobs(&mut h);
    assert!(dir.path().join("report.pdf").exists());
}

#[test]
fn a_saved_search_runs_again_from_the_navigation_pane() {
    let dir = picture_folder();
    let docs = dir.path().join("Docs");
    std::fs::create_dir(&docs).unwrap();
    std::fs::write(docs.join("invoice-march.pdf"), "x").unwrap();
    std::fs::write(docs.join("letter.txt"), "x").unwrap();
    let mut h = harness(&docs);
    h.state_mut().ui.search_text = "invoice".into();
    h.run_steps(2);
    h.get_by_label("☆").click();
    h.run_steps(2);
    assert_eq!(h.state().ui.settings.saved_searches.len(), 1);

    // Elsewhere, with the search cleared, one click brings it back.
    h.state_mut().fs.set_path(dir.path().to_path_buf());
    h.run_steps(4);
    assert!(h.state().ui.search_text.is_empty());
    h.get_by_label("invoice in Docs").click();
    h.run_steps(6);
    assert_eq!(h.state().fs.get_current_path(), docs);
    assert_eq!(h.state().ui.search_text, "invoice");
    let names: Vec<_> = h
        .state()
        .ui
        .list
        .items
        .iter()
        .map(|i| i.name.clone())
        .collect();
    assert_eq!(names, ["invoice-march.pdf"]);
}

#[test]
fn the_tree_opens_out_to_the_folder_shown_and_pins_reorder() {
    let dir = picture_folder();
    let projects = dir.path().join("Projects");
    let deep = projects.join("2026/report");
    std::fs::create_dir_all(&deep).unwrap();
    let other = dir.path().join("Other");
    std::fs::create_dir(&other).unwrap();
    let mut h = harness(dir.path());
    h.state_mut().ui.settings.pinned = vec![projects.clone(), other.clone()];
    h.run_steps(2);

    h.state_mut().fs.set_path(deep.clone());
    h.run_steps(6);
    let tree = &h.state().ui.tree;
    assert!(tree.is_expanded(&projects) && tree.is_expanded(&projects.join("2026")));
    assert!(
        !tree.is_expanded(&deep),
        "the open folder itself stays closed"
    );

    let projects_row = leftmost(&h, "Projects");
    right_click(&mut h, projects_row.center());
    h.get_by_label("⬇ Move down").click();
    h.run_steps(2);
    assert_eq!(h.state().ui.settings.pinned, [other, projects]);
}

#[cfg(unix)]
#[test]
fn eject_from_the_drive_menu_steps_off_the_drive_first() {
    let _serial = crate::eject::TEST_SERIAL
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    use std::os::unix::fs::PermissionsExt;
    let media = tempfile::tempdir().unwrap();
    let usb = media.path().join("USB");
    std::fs::create_dir(&usb).unwrap();
    let tools = tempfile::tempdir().unwrap();
    let tool = tools.path().join("eject");
    let log = tools.path().join("ejected");
    std::fs::write(
        &tool,
        format!("#!/bin/sh\necho \"$1\" > {}\n", log.display()),
    )
    .unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    *crate::eject::TEST_TOOL.lock().unwrap() = Some(tool.to_string_lossy().to_string());

    let mut h = harness(&usb);
    *h.state().ui.drives.lock().unwrap() = vec![DriveInfo {
        path: usb.clone(),
        label: "USB stick".into(),
        drive_type: crate::file_system::DriveType::Removable,
        total_space: Some(8 << 30),
        free_space: Some(4 << 30),
    }];
    h.run_steps(2);
    let drive = h.get_by_label("USB stick").rect();
    right_click(&mut h, drive.center());
    h.get_by_label("⏏ Eject").click();
    h.run_steps(1);
    wait_for_jobs(&mut h);
    *crate::eject::TEST_TOOL.lock().unwrap() = None;

    assert_eq!(
        std::fs::read_to_string(&log).unwrap().trim(),
        usb.to_string_lossy()
    );
    assert!(!h.state().fs.get_current_path().starts_with(&usb));
}

#[test]
fn compress_to_zip_then_extract_all_from_the_menu() {
    let dir = picture_folder();
    let mut h = harness(dir.path());
    let notes = rightmost(&h, "notes.txt");
    right_click(&mut h, notes.center());
    h.get_by_label("🗜 Compress to ZIP file").click();
    h.run_steps(1);
    wait_for_jobs(&mut h);
    let zip = dir.path().join("notes.zip");
    assert!(zip.exists());

    h.run_steps(3);
    let zip_item = rightmost(&h, "notes.zip");
    right_click(&mut h, zip_item.center());
    h.get_by_label("📦 Extract all").click();
    h.run_steps(1);
    wait_for_jobs(&mut h);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("notes/notes.txt")).unwrap(),
        "hello"
    );
}

#[test]
fn a_picture_becomes_the_desktop_background_from_its_menu() {
    let dir = picture_folder();
    let mut h = harness(dir.path());
    let photo = h
        .state()
        .ui
        .list
        .items
        .iter()
        .find(|i| crate::thumbnail::ThumbnailCache::is_image_file(&i.path))
        .unwrap()
        .clone();
    let tile = rightmost(&h, &photo.name);
    right_click(&mut h, tile.center());
    h.get_by_label("🖼 Set as desktop background").click();
    h.run_steps(2);
    let real = std::fs::canonicalize(&photo.path).unwrap();
    assert!(crate::wallpaper::TEST_RAN.lock().unwrap().contains(&real));
}

#[test]
fn apply_to_all_folders_sets_the_view_for_that_kind_of_folder() {
    let dir = picture_folder();
    let docs = dir.path().join("Docs");
    let other = dir.path().join("Other");
    std::fs::create_dir(&docs).unwrap();
    std::fs::create_dir(&other).unwrap();
    std::fs::write(docs.join("a.txt"), "x").unwrap();
    std::fs::write(other.join("b.txt"), "x").unwrap();
    let mut h = harness(&docs);
    h.run_steps(3);
    h.state_mut().ui.view.mode = ViewMode::Icons;
    {
        let state = h.state_mut();
        let ctx = egui::Context::default();
        state.ui.apply(Action::ApplyViewToAll, &ctx, &mut state.fs);
    }
    h.state_mut().fs.set_path(other.clone());
    h.run_steps(4);
    assert_eq!(
        h.state().ui.view.mode,
        ViewMode::Icons,
        "other folders follow"
    );
    // Picture folders keep their own template (large icons).
    h.state_mut().fs.set_path(dir.path().to_path_buf());
    h.run_steps(4);
    assert_eq!(h.state().ui.view.icon_size, 160.0);
}

#[test]
fn item_check_boxes_select_several_without_ctrl() {
    let dir = picture_folder();
    let mut h = harness(dir.path());
    h.state_mut().ui.settings.item_checkboxes = true;
    h.state_mut().ui.view.mode = ViewMode::Details;
    h.run_steps(3);
    let names: Vec<String> = h
        .state()
        .ui
        .list
        .items
        .iter()
        .take(2)
        .map(|i| i.name.clone())
        .collect();
    for name in &names {
        let row = rightmost(&h, name);
        // The box sits at the row's left edge.
        mouse_click(&mut h, Pos2::new(row.left() + 11.0, row.center().y));
        h.run_steps(2);
    }
    assert_eq!(h.state().ui.selection.len(), 2, "both ticked");
    // A plain click elsewhere on a row still selects just that one.
    let row = rightmost(&h, &names[0]);
    mouse_click(&mut h, row.center());
    h.run_steps(2);
    assert_eq!(h.state().ui.selection.len(), 1);
}

#[test]
fn the_theme_and_interface_size_change_from_the_view_menu_without_a_restart() {
    let dir = picture_folder();
    let mut h = harness(dir.path());
    open_menu(&mut h, &["View", "Theme", "Light"]);
    h.run_steps(2);
    assert_eq!(
        h.state().ui.settings.appearance,
        crate::theme::Appearance::Light
    );
    assert_eq!(
        crate::theme::pal(),
        crate::theme::LIGHT,
        "drawn light from the next frame"
    );
    open_menu(&mut h, &["View", "Interface Size", "125%"]);
    h.run_steps(3);
    assert_eq!(h.state().ui.settings.ui_scale, 1.25);
    assert_eq!(h.ctx.zoom_factor(), 1.25);
    open_menu(&mut h, &["View", "Theme", "High contrast dark"]);
    h.run_steps(2);
    assert_eq!(crate::theme::pal(), crate::theme::HIGH_CONTRAST_DARK);
}

#[test]
fn bulk_rename_dialog_previews_renames_and_undoes_in_one_step() {
    let dir = picture_folder();
    let mut h = harness(dir.path());
    h.get_by_label("IMG_1.jpg").click();
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.run_steps(2);
    h.key_press(Key::F2);
    h.run_steps(3);
    assert!(matches!(h.state().ui.dialog, Some(Dialog::BulkRename(_))));
    // The preview shows the classic "Name (n)" names before anything changes.
    h.get_by_label("IMG_1 (6).txt");
    assert!(dir.path().join("notes.txt").exists());
    h.get_by_label("Rename").click();
    h.run_steps(3);
    assert!(h.state().ui.dialog.is_none());
    let names = |dir: &Path| {
        let mut n: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        n.sort();
        n
    };
    assert_eq!(
        names(dir.path()),
        [
            "IMG_1 (1).jpg",
            "IMG_1 (2).jpg",
            "IMG_1 (3).jpg",
            "IMG_1 (4).jpg",
            "IMG_1 (5).jpg",
            "IMG_1 (6).txt"
        ]
    );
    h.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    h.run_steps(1);
    wait_for_jobs(&mut h);
    assert_eq!(
        names(dir.path()),
        [
            "IMG_1.jpg",
            "IMG_2.jpg",
            "IMG_3.jpg",
            "IMG_4.jpg",
            "IMG_5.jpg",
            "notes.txt"
        ]
    );
}

// Ninimma modification: render a real semantic-search demo, using only made-up files.
#[cfg(all(feature = "readme-screenshot", feature = "smart-search"))]
#[test]
#[ignore = "requires NINIMMA_DEMO_MODEL and a headless graphics adapter"]
fn readme_smart_search_surveying_equipment() {
    let root = tempfile::tempdir().unwrap();
    let collection = root.path().join("Field collection");
    std::fs::create_dir(&collection).unwrap();
    for (name, text) in [
        ("Total station field guide.txt", "A total station measures horizontal and vertical angles and distances for land surveys. Mount the instrument on a tripod, level it with the bubble, and sight a prism to record coordinates. Use a measuring rod to establish reference points."),
        ("Optical level checklist.txt", "An optical level, tripod and graduated levelling staff establish differences in ground elevation. Check the circular bubble and focus the telescope before taking a backsight reading. Record site measurements in the field notebook."),
        ("GNSS receiver setup.txt", "A GNSS base station and rover receiver use satellite positioning to locate control points on construction sites and map land boundaries. Check the antenna pole height and radio connection before collecting coordinates."),
        ("Measuring rod calibration.txt", "Inspect the graduated measuring rod and levelling staff for straightness. Compare its divisions with a reference tape. These instruments are used to measure terrain heights and set out building foundations."),
        ("Kitchen inventory.txt", "Kitchen cupboards contain plates, cups, saucepans and cutlery. Keep coffee, tea and flour in labelled containers."),
    ] {
        std::fs::write(collection.join(name), text).unwrap();
    }
    let models = crate::models::models_dir().unwrap();
    std::fs::create_dir_all(&models).unwrap();
    let model = models.join(crate::models::EMBEDDING_MODEL.file);
    assert!(
        !model.exists(),
        "run the screenshot test in its own process"
    );
    std::fs::copy(
        std::env::var_os("NINIMMA_DEMO_MODEL").expect("set NINIMMA_DEMO_MODEL"),
        &model,
    )
    .unwrap();
    let mut h = harness_sized(&collection, Vec2::new(1400.0, 880.0));
    let index_dir = tempfile::tempdir().unwrap();
    {
        let app = h.state_mut();
        app.ui.settings.appearance = crate::theme::Appearance::Dark;
        app.ui.index.test_setup = Some((
            index_dir.path().join("index"),
            Arc::new(crate::security::key_store::MemoryKeyStore::default()),
        ));
        app.ui.settings.index = IndexSettings {
            enabled: true,
            roots: vec![collection.clone()],
            mode: crate::index::config::IndexMode::Smart,
            pause_on_battery: false,
            pictures: false,
            smart_acknowledged: true,
            ..Default::default()
        };
        app.ui.settings.search_scope = SearchScope::Everywhere;
        app.ui.view.mode = ViewMode::Details;
        app.ui.settings.show_preview_pane = true;
    }
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        h.step();
        if h.state()
            .ui
            .index
            .status
            .as_ref()
            .is_some_and(|s| s.vectors >= 5 && s.smart_problem.is_none())
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "demo index failed: {:?}",
            h.state().ui.index.status
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    h.state_mut().ui.search_text = "surveying equipment".into();
    step_until(&mut h, "semantic search results", |app| {
        !app.ui.index.results.searching && !app.ui.list.items.is_empty()
    });
    assert!(
        h.state()
            .ui
            .index
            .results
            .reasons
            .values()
            .any(|reason| matches!(reason, crate::index::types::MatchReason::Meaning(_))),
        "the screenshot must show actual meaning-based matches"
    );
    println!("Demo results: {:?}", listed(h.state()));
    let selected = h
        .state()
        .ui
        .list
        .items
        .iter()
        .find(|item| item.name == "Total station field guide.txt")
        .unwrap()
        .path
        .clone();
    h.state_mut().ui.selection.select_only(&selected);
    h.run_steps(25);
    let output = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("assets/screenshots/smart-search-surveying.png");
    std::fs::create_dir_all(output.parent().unwrap()).unwrap();
    h.render().unwrap().save(&output).unwrap();
    drop(h);
    std::fs::remove_file(model).unwrap();
    println!("Screenshot: {}", output.display());
}

#[test]
fn windows_shift_delete_still_requires_confirmation_in_file_view() {
    let dir = picture_folder();
    let mut h = harness(dir.path());
    h.ctx.set_os(egui::os::OperatingSystem::Windows);
    h.get_by_label("notes.txt").click();
    h.run_steps(2);
    let mut input = egui::RawInput::default();
    input.events.push(Event::Key {
        key: Key::Delete,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Modifiers::SHIFT,
    });
    crate::system_clipboard::prepare_input(&h.ctx, &mut input);
    for event in input.events {
        h.event(event);
    }
    h.run_steps(2);
    assert!(matches!(
        &h.state().ui.dialog,
        Some(Dialog::ConfirmDelete { .. })
    ));
    assert!(
        h.state().ui.clipboard.is_none(),
        "Shift+Delete must not cut files"
    );
    assert!(
        dir.path().join("notes.txt").exists(),
        "confirmation has not been accepted"
    );
}

#[test]
fn oversized_archive_requires_confirmation_and_cancel_writes_nothing() {
    let folder = tempfile::tempdir().unwrap();
    let one = folder.path().join("one.txt");
    let two = folder.path().join("two.txt");
    std::fs::write(&one, b"one").unwrap();
    std::fs::write(&two, b"two").unwrap();
    let zip = folder.path().join("large.zip");
    crate::archive::compress(&[one, two], &zip).unwrap();
    let mut data = std::fs::read(&zip).unwrap();
    let positions: Vec<_> = data
        .windows(4)
        .enumerate()
        .filter_map(|(i, b)| (b == b"PK\x01\x02").then_some(i))
        .collect();
    assert_eq!(positions.len(), 2);
    for i in positions {
        data[i + 24..i + 28].copy_from_slice(&(3 * 1024 * 1024 * 1024u32).to_le_bytes());
    }
    std::fs::write(&zip, data).unwrap();
    let mut h = harness(folder.path());
    let ctx = h.ctx.clone();
    let app = h.state_mut();
    app.ui.apply(Action::Extract(zip), &ctx, &mut app.fs);
    h.run_steps(3);
    h.get_by_label("Extract a large archive?");
    assert!(!folder.path().join("large").exists());
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert!(h.state().ui.dialog.is_none());
    assert!(!folder.path().join("large").exists());
}

#[test]
fn approved_archive_extraction_keeps_a_finite_output_budget() {
    let folder = tempfile::tempdir().unwrap();
    let source = folder.path().join("source.txt");
    std::fs::write(&source, b"ten bytes!").unwrap();
    let zip = folder.path().join("approved.zip");
    crate::archive::compress(&[source], &zip).unwrap();
    let mut h = harness(folder.path());
    h.state_mut().ui.dialog = Some(Dialog::ConfirmLargeArchive {
        archive: zip,
        bytes: 10,
    });
    h.run_steps(3);
    h.get_by_label("Extract larger archive").click();
    let output = folder.path().join("approved/source.txt");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        h.run_steps(3);
        if std::fs::read(&output).ok().as_deref() == Some(b"ten bytes!") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "approved extraction never completed"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn about_displays_geonames_and_bundled_model_notices() {
    let folder = tempfile::tempdir().unwrap();
    let mut h = harness(folder.path());
    h.state_mut().ui.dialog = Some(Dialog::About);
    h.run_steps(3);
    h.get_by_label("GeoNames");
    h.get_by_label("CC BY 4.0");
    h.get_by_label("Licenses, model downloads and third-party notices")
        .click();
    h.run_steps(3);
    h.get_by_label_contains("Picture search downloads OpenAI CLIP ViT-B/32");
}

#[test]
fn voice_controls_open_and_reopen_without_recording() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = harness(dir.path());
    for _ in 0..2 {
        h.get_by_label("Voice").click();
        h.run_steps(2);
        h.get_by_label("Dictate");
        h.get_by_label("Voice").click();
        h.run_steps(2);
    }
}
