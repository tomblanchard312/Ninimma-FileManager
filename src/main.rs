// Ninimma modification: independent project branding and integration.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use eframe::egui;
use std::path::PathBuf;

mod archive;
#[cfg(test)]
mod bench;
mod bookmarks;
mod bulk_rename;
#[cfg(all(unix, not(target_os = "macos")))]
mod desktop_default;
mod disk_images;
mod dnd;
mod eject;
mod embedding;
#[cfg(all(unix, not(target_os = "macos")))]
mod file_manager1;
mod file_ops;
mod file_system;
mod folder_tree;
mod fonts;
mod image_ops;
mod index;
mod index_ui;
mod models;
mod network;
mod ocr;
#[cfg(all(unix, not(target_os = "macos")))]
mod open_with;
mod pictures;
mod preview;
mod properties;
mod recent;
mod security;
mod selection;
mod system_clipboard;
#[cfg(all(unix, not(target_os = "macos")))]
mod terminal;
mod theme;
mod thumbnail;
mod trash_bin;
mod ui;
mod understand;
mod undo;
mod wallpaper;

use file_system::FileSystem;
use ui::{NinimmaUI, Settings};

const SETTINGS_KEY: &str = "settings";

/// When the process started, for NINIMMA_STARTUP_PROBE.
static STARTED: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

fn main() -> eframe::Result<()> {
    STARTED.get_or_init(std::time::Instant::now);
    // Lets packaging checks confirm the binary runs without opening a window.
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("ninimma {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    if let Some(arg) = std::env::args().nth(1) {
        if arg == "--make-default" || arg == "--restore-default" {
            match desktop_default::change(arg == "--restore-default") {
                Ok(message) => println!("{message}"),
                Err(error) => {
                    eprintln!("{error}");
                    std::process::exit(1);
                }
            }
            return Ok(());
        }
    }

    // A folder passed on the command line opens there ("ninimma ~/Pictures").
    let arg_path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .filter(|p| p.is_dir());

    // Windows draws with Direct3D 12 (wgpu), which virtual machines without
    // OpenGL 2 have too (or Microsoft's WARP software renderer); OpenGL is
    // the fallback. Elsewhere, OpenGL. NINIMMA_RENDERER=glow or =wgpu
    // forces one.
    #[cfg(windows)]
    {
        const RENDERER: &str = "NINIMMA_RENDERER";
        match std::env::var(RENDERER).as_deref() {
            Ok("glow") => run(eframe::Renderer::Glow, arg_path),
            Ok("wgpu") => run(eframe::Renderer::Wgpu, arg_path),
            _ => {
                if run(eframe::Renderer::Wgpu, arg_path).is_ok() {
                    return Ok(());
                }
                // winit allows one event loop per process, and wgpu's
                // already used it: OpenGL starts in a fresh copy of the app.
                let status = std::env::current_exe()
                    .and_then(|exe| {
                        std::process::Command::new(exe)
                            .args(std::env::args_os().skip(1))
                            .env(RENDERER, "glow")
                            .status()
                    })
                    .map_err(|e| eframe::Error::AppCreation(Box::new(e)))?;
                std::process::exit(status.code().unwrap_or(1))
            }
        }
    }
    #[cfg(not(windows))]
    run(eframe::Renderer::Glow, arg_path)
}

fn run(renderer: eframe::Renderer, arg_path: Option<PathBuf>) -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1400.0, 900.0])
            // Small enough for a 1280x768 screen at 150% scaling (853x512
            // points); a bigger window is shrunk to fit (`fit_to_monitor`).
            .with_min_inner_size([480.0, 360.0])
            .with_title("Ninimma")
            // Matches the desktop file's name, so shells group the window
            // with its icon; the Flatpak build sets its reverse-DNS id.
            .with_app_id(option_env!("NINIMMA_APP_ID").unwrap_or("ninimma"))
            .with_icon(app_icon()),
        persist_window: true,
        renderer,
        ..Default::default()
    };
    eframe::run_native(
        "Ninimma",
        options,
        Box::new(move |cc| {
            let settings: Settings = cc
                .storage
                .and_then(|s| eframe::get_value(s, SETTINGS_KEY))
                .unwrap_or_default();
            configure_style(&cc.egui_ctx);
            Ok(Box::new(NinimmaApp::new(settings, arg_path)))
        }),
    )
}

/// Window/taskbar icon (source: assets/icon.svg).
fn app_icon() -> egui::IconData {
    let png = include_bytes!("../assets/icons/ninimma-256.png");
    let img = image::load_from_memory(png)
        .expect("bundled icon is a valid PNG")
        .to_rgba8();
    egui::IconData {
        width: img.width(),
        height: img.height(),
        rgba: img.into_raw(),
    }
}

fn configure_style(ctx: &egui::Context) {
    fonts::install_icons(ctx);
    // Colours and size come from the chosen theme (`theme::apply`, each
    // frame). Ctrl +/- and Ctrl+wheel resize icons, as in Explorer, rather than
    // zooming the whole interface.
    ctx.options_mut(|o| o.zoom_with_keyboard = false);
}

struct NinimmaApp {
    ui: NinimmaUI,
    fs: FileSystem,
    /// The window has been checked against its monitor (once, at startup).
    fitted: bool,
    /// Startup time has been reported (NINIMMA_STARTUP_PROBE).
    probed: bool,
}

/// Where and how big the window should be to fit on its monitor, if it
/// doesn't already: a size saved on a bigger screen, or a high scaling
/// factor, can leave it taller than the screen with its title and menu
/// bar off the top. All in points; `outer` includes the title bar.
fn fit_to_monitor(
    outer: egui::Rect,
    inner: egui::Rect,
    monitor: egui::Vec2,
) -> Option<(egui::Vec2, egui::Pos2)> {
    let fits = outer.min.x >= 0.0
        && outer.min.y >= 0.0
        && outer.max.x <= monitor.x
        && outer.max.y <= monitor.y;
    if fits {
        return None;
    }
    // Room for the window's frame, and a margin for panels.
    let frame = outer.size() - inner.size();
    let room = (monitor - frame - egui::vec2(16.0, 48.0)).max(egui::vec2(320.0, 240.0));
    let size = inner.size().min(room);
    let width = size.x + frame.x;
    let x = outer.min.x.clamp(0.0, (monitor.x - width).max(0.0));
    Some((size, egui::pos2(x, 0.0)))
}

impl NinimmaApp {
    fn new(settings: Settings, arg_path: Option<PathBuf>) -> Self {
        let start_path = arg_path
            // Reopen the tab that was active, so the other saved tabs restore around it.
            .or_else(|| {
                settings
                    .open_tabs
                    .get(settings.active_tab)
                    .cloned()
                    .filter(|p| p.is_dir())
            })
            .or_else(|| settings.last_path.clone().filter(|p| p.is_dir()))
            .or_else(file_system::home_dir)
            .unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "C:\\" } else { "/" }));
        let fs = FileSystem::new(start_path, settings.show_hidden);
        let ui = NinimmaUI::new(settings, &fs);
        Self {
            ui,
            fs,
            fitted: false,
            probed: false,
        }
    }
}

impl eframe::App for NinimmaApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if !self.fitted {
            let (outer, inner, monitor) = ui.ctx().input(|i| {
                let v = i.viewport();
                (v.outer_rect, v.inner_rect, v.monitor_size)
            });
            // Known once the window is up; checked once.
            if let (Some(outer), Some(inner), Some(monitor)) = (outer, inner, monitor) {
                self.fitted = true;
                if let Some((size, position)) = fit_to_monitor(outer, inner, monitor) {
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::OuterPosition(position));
                }
            }
        }
        self.ui.show(ui, &mut self.fs);
        // tools/bench.sh times startup: from launch to the first frame
        // with the folder listed.
        if !self.probed
            && std::env::var_os("NINIMMA_STARTUP_PROBE").is_some()
            && !self.fs.is_loading()
        {
            self.probed = true;
            if let Some(start) = STARTED.get() {
                eprintln!("STARTUP {:.1} ms", start.elapsed().as_secs_f64() * 1000.0);
            }
        }
        if let Some((paths, move_files)) = self.ui.take_drag_out() {
            let flag = self.ui.drag_out_flag();
            if let Err(e) = dnd::start_drag(frame, ui.ctx(), paths, move_files, flag) {
                self.ui.report(e);
            }
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, SETTINGS_KEY, &self.ui.settings(&self.fs));
    }
}

#[cfg(test)]
mod window_tests {
    use super::fit_to_monitor;
    use egui::{pos2, vec2, Rect};

    #[test]
    fn a_window_taller_than_the_screen_is_shrunk_and_moved_on_screen() {
        // The FreeBSD VM: 1280x768 pixels at 150% is 853x512 points; the old
        // 800x600 minimum plus a title bar didn't fit.
        let monitor = vec2(853.0, 512.0);
        let outer = Rect::from_min_size(pos2(0.0, -40.0), vec2(853.0, 619.0));
        let inner = Rect::from_min_size(pos2(0.0, -21.0), vec2(853.0, 600.0));
        let (size, at) = fit_to_monitor(outer, inner, monitor).expect("needs fitting");
        assert!(size.y + 19.0 <= monitor.y - 40.0, "{size:?}");
        assert!(size.x + 0.0 <= monitor.x, "{size:?}");
        assert_eq!(at.y, 0.0, "title bar back on screen");
        // A window that fits is left alone.
        let ok = Rect::from_min_size(pos2(100.0, 50.0), vec2(700.0, 400.0));
        let ok_inner = Rect::from_min_size(pos2(100.0, 69.0), vec2(700.0, 381.0));
        assert_eq!(fit_to_monitor(ok, ok_inner, monitor), None);
        // Too wide (saved on a big monitor): narrowed, kept on screen.
        let wide = Rect::from_min_size(pos2(1200.0, 0.0), vec2(1900.0, 500.0));
        let wide_inner = Rect::from_min_size(pos2(1200.0, 19.0), vec2(1900.0, 481.0));
        let (size, at) = fit_to_monitor(wide, wide_inner, vec2(1600.0, 900.0)).unwrap();
        assert!(
            size.x <= 1600.0 && at.x + size.x <= 1600.0,
            "{size:?} at {at:?}"
        );
    }
}
