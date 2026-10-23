// Ninimma modification: independent project branding and integration.
//! Dragging files out of the window to other applications.
//!
//! winit (and so egui) can receive file drops but can't start a drag, so the
//! source side is implemented per platform:
//! * X11: the XDND protocol, spoken from a helper thread over its own X
//!   connection (no GTK needed).
//! * Windows / macOS: the `drag` crate (OLE DoDragDrop / NSDraggingSession).
//! * Wayland: not supported yet.
//!
//! Drops back onto our own window arrive through winit like any other file
//! drop, so internal drag-and-drop between folders uses the same path.

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
#[cfg(all(unix, not(target_os = "macos")))]
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Start dragging `paths`. `active` is cleared when the drag ends.
/// Returns an error message if this platform/session can't drag out.
pub fn start_drag(
    window: &impl HasWindowHandle,
    ctx: &egui::Context,
    paths: Vec<PathBuf>,
    move_files: bool,
    active: Arc<AtomicBool>,
) -> Result<(), String> {
    let raw = window.window_handle().map_err(|e| e.to_string())?.as_raw();
    active.store(true, Ordering::Relaxed);
    let result = match raw {
        #[cfg(all(unix, not(target_os = "macos")))]
        RawWindowHandle::Xlib(_) | RawWindowHandle::Xcb(_) => {
            x11::start(paths, move_files, active.clone(), ctx.clone())
        }
        #[cfg(any(windows, target_os = "macos"))]
        RawWindowHandle::Win32(_) | RawWindowHandle::AppKit(_) => {
            native::start(window, paths, move_files, active.clone(), ctx.clone())
        }
        _ => {
            let _ = (paths, move_files, ctx);
            Err("Dragging files to other apps isn't supported on Wayland yet".to_string())
        }
    };
    if result.is_err() {
        active.store(false, Ordering::Relaxed);
    }
    result
}

/// `file:///…` URI with everything outside RFC 3986's unreserved set
/// (plus `/`) percent-encoded.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(any(windows, target_os = "macos"))]
mod native {
    use super::*;

    pub fn start(
        window: &impl HasWindowHandle,
        paths: Vec<PathBuf>,
        move_files: bool,
        active: Arc<AtomicBool>,
        ctx: egui::Context,
    ) -> Result<(), String> {
        let options = drag::Options {
            mode: if move_files {
                drag::DragMode::Move
            } else {
                drag::DragMode::Copy
            },
            ..Default::default()
        };
        let done = active.clone();
        drag::start_drag(
            window,
            drag::DragItem::Files(paths),
            drag::Image::Raw(drag_icon_png()),
            move |_result, _pos| {
                done.store(false, Ordering::Relaxed);
                ctx.request_repaint();
            },
            options,
        )
        .map_err(|e| format!("Couldn't start drag: {e}"))
    }

    /// A small file-shaped image shown under the cursor while dragging.
    fn drag_icon_png() -> Vec<u8> {
        let img = image::RgbaImage::from_fn(40, 48, |x, y| {
            let edge = x == 0 || y == 0 || x == 39 || y == 47;
            if edge {
                image::Rgba([60, 150, 230, 255])
            } else {
                image::Rgba([235, 240, 248, 230])
            }
        });
        let mut out = std::io::Cursor::new(Vec::new());
        let _ = img.write_to(&mut out, image::ImageFormat::Png);
        out.into_inner()
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod x11 {
    use super::*;
    use std::time::{Duration, Instant};
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{
        AtomEnum, ClientMessageEvent, ConnectionExt as _, CreateWindowAux, EventMask, KeyButMask,
        PropMode, SelectionNotifyEvent, SelectionRequestEvent, Window, WindowClass,
        SELECTION_NOTIFY_EVENT,
    };
    use x11rb::protocol::Event;
    use x11rb::rust_connection::RustConnection;
    use x11rb::wrapper::ConnectionExt as _;
    use x11rb::{CURRENT_TIME, NONE};

    const XDND_VERSION: u32 = 5;
    const POLL: Duration = Duration::from_millis(10);

    x11rb::atom_manager! {
        Atoms: AtomsCookie {
            XdndAware, XdndProxy, XdndEnter, XdndPosition, XdndStatus, XdndLeave, XdndDrop,
            XdndFinished, XdndSelection, XdndTypeList, XdndActionCopy, XdndActionMove,
            TARGETS, UTF8_STRING, ATOM,
            URI_LIST: b"text/uri-list",
            GNOME_FILES: b"x-special/gnome-copied-files",
            TEXT_PLAIN: b"text/plain;charset=utf-8",
        }
    }

    pub fn start(
        paths: Vec<PathBuf>,
        move_files: bool,
        active: Arc<AtomicBool>,
        ctx: egui::Context,
    ) -> Result<(), String> {
        let (conn, screen) =
            x11rb::connect(None).map_err(|e| format!("Can't connect to X server: {e}"))?;
        std::thread::spawn(move || {
            let mut source = Source {
                conn,
                screen,
                paths,
                move_files,
                atoms: None,
            };
            if let Err(e) = source.run() {
                eprintln!("drag and drop failed: {e}");
            }
            active.store(false, Ordering::Relaxed);
            ctx.request_repaint();
        });
        Ok(())
    }

    #[derive(Clone, Copy, PartialEq)]
    struct Target {
        window: Window,
        /// Where to send messages (differs from `window` with XdndProxy).
        proxy: Window,
        version: u32,
    }

    struct Source {
        conn: RustConnection,
        screen: usize,
        paths: Vec<PathBuf>,
        move_files: bool,
        atoms: Option<Atoms>,
    }

    type Res<T> = Result<T, Box<dyn std::error::Error>>;

    impl Source {
        fn atoms(&self) -> Atoms {
            self.atoms.expect("interned")
        }

        fn run(&mut self) -> Res<()> {
            self.atoms = Some(Atoms::new(&self.conn)?.reply()?);
            let a = self.atoms();
            let root = self.conn.setup().roots[self.screen].root;

            // An invisible window to own the selection and receive replies.
            let win = self.conn.generate_id()?;
            self.conn.create_window(
                0,
                win,
                root,
                -10,
                -10,
                1,
                1,
                0,
                WindowClass::INPUT_ONLY,
                0,
                &CreateWindowAux::new().override_redirect(1),
            )?;
            let types = [a.URI_LIST, a.GNOME_FILES, a.UTF8_STRING];
            self.conn.change_property32(
                PropMode::REPLACE,
                win,
                a.XdndTypeList,
                AtomEnum::ATOM,
                &types,
            )?;
            self.conn
                .set_selection_owner(win, a.XdndSelection, CURRENT_TIME)?;
            self.conn.flush()?;

            let mut current: Option<Target> = None;
            let mut accepted = false;
            let mut awaiting_status = false;
            let mut last_pos = (i16::MIN, i16::MIN);
            let mut last_sent = Instant::now();
            let started = Instant::now();

            loop {
                while let Some(event) = self.conn.poll_for_event()? {
                    match event {
                        Event::SelectionRequest(req) => self.answer(&req)?,
                        Event::ClientMessage(msg) if msg.type_ == a.XdndStatus => {
                            let data = msg.data.as_data32();
                            if current.is_some_and(|t| t.window == data[0]) {
                                accepted = data[1] & 1 == 1;
                                awaiting_status = false;
                            }
                        }
                        _ => {}
                    }
                }

                let pointer = self.conn.query_pointer(root)?.reply()?;
                let pos = (pointer.root_x, pointer.root_y);
                let button_down = u16::from(pointer.mask) & u16::from(KeyButMask::BUTTON1) != 0;
                // Give the button a moment: the drag may start just as it's released.
                if !button_down && started.elapsed() > Duration::from_millis(30) {
                    return self.finish(win, current, accepted);
                }

                let target = self.find_target(root, &pointer)?;
                if target != current {
                    if let Some(old) = current {
                        self.send(old, a.XdndLeave, [win, 0, 0, 0, 0])?;
                    }
                    if let Some(new) = target {
                        let version = new.version.min(XDND_VERSION);
                        self.send(
                            new,
                            a.XdndEnter,
                            [win, version << 24, types[0], types[1], types[2]],
                        )?;
                    }
                    current = target;
                    accepted = false;
                    awaiting_status = false;
                    last_pos = (i16::MIN, i16::MIN);
                }
                // Send a position update when the pointer moves, but don't flood
                // a target that hasn't answered the previous one.
                if let Some(t) = current {
                    let stale = last_sent.elapsed() > Duration::from_millis(150);
                    if (pos != last_pos && !awaiting_status) || stale {
                        let action = if self.move_files {
                            a.XdndActionMove
                        } else {
                            a.XdndActionCopy
                        };
                        let xy = ((pos.0 as u32) << 16) | (pos.1 as u16 as u32);
                        self.send(t, a.XdndPosition, [win, 0, xy, CURRENT_TIME, action])?;
                        last_pos = pos;
                        last_sent = Instant::now();
                        awaiting_status = true;
                    }
                }
                self.conn.flush()?;
                std::thread::sleep(POLL);
            }
        }

        /// Button released: drop on an accepting target, otherwise cancel.
        fn finish(&self, win: Window, target: Option<Target>, accepted: bool) -> Res<()> {
            let a = self.atoms();
            let Some(t) = target else { return Ok(()) };
            if !accepted {
                self.send(t, a.XdndLeave, [win, 0, 0, 0, 0])?;
                self.conn.flush()?;
                return Ok(());
            }
            self.send(t, a.XdndDrop, [win, 0, CURRENT_TIME, 0, 0])?;
            self.conn.flush()?;
            // Serve the target's data request(s) until it says it's finished.
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                while let Some(event) = self.conn.poll_for_event()? {
                    match event {
                        Event::SelectionRequest(req) => self.answer(&req)?,
                        Event::ClientMessage(msg) if msg.type_ == a.XdndFinished => return Ok(()),
                        _ => {}
                    }
                }
                self.conn.flush()?;
                std::thread::sleep(POLL);
            }
            Ok(())
        }

        /// The innermost XDND-aware window under the pointer.
        fn find_target(
            &self,
            root: Window,
            pointer: &x11rb::protocol::xproto::QueryPointerReply,
        ) -> Res<Option<Target>> {
            let mut child = pointer.child;
            while child != NONE {
                if let Some(version) = self.xdnd_version(child)? {
                    let proxy = self
                        .window_property(child, self.atoms().XdndProxy)?
                        .unwrap_or(child);
                    return Ok(Some(Target {
                        window: child,
                        proxy,
                        version,
                    }));
                }
                child = self.conn.query_pointer(child)?.reply()?.child;
            }
            let _ = root;
            Ok(None)
        }

        fn xdnd_version(&self, w: Window) -> Res<Option<u32>> {
            let reply = self
                .conn
                .get_property(false, w, self.atoms().XdndAware, AtomEnum::ATOM, 0, 1)?
                .reply()?;
            Ok(reply
                .value32()
                .and_then(|mut v| v.next())
                .filter(|&v| v >= 3))
        }

        fn window_property(&self, w: Window, prop: u32) -> Res<Option<Window>> {
            let reply = self
                .conn
                .get_property(false, w, prop, AtomEnum::WINDOW, 0, 1)?
                .reply()?;
            Ok(reply
                .value32()
                .and_then(|mut v| v.next())
                .filter(|&v| v != NONE))
        }

        fn send(&self, t: Target, kind: u32, mut data: [u32; 5]) -> Res<()> {
            if kind == self.atoms().XdndEnter {
                data[1] = (data[1] & 0xFF00_0000) | (t.version.min(XDND_VERSION) << 24);
            }
            let msg = ClientMessageEvent::new(32, t.window, kind, data);
            self.conn
                .send_event(false, t.proxy, EventMask::NO_EVENT, msg)?;
            Ok(())
        }

        /// Hand over the dragged files in whichever format the target asked for.
        fn answer(&self, req: &SelectionRequestEvent) -> Res<()> {
            let a = self.atoms();
            let property = if req.property == NONE {
                req.target
            } else {
                req.property
            };
            let uris: Vec<String> = self.paths.iter().map(|p| file_uri(p)).collect();
            let ok = if req.target == a.TARGETS {
                let targets = [
                    a.TARGETS,
                    a.URI_LIST,
                    a.GNOME_FILES,
                    a.UTF8_STRING,
                    a.TEXT_PLAIN,
                ];
                self.conn.change_property32(
                    PropMode::REPLACE,
                    req.requestor,
                    property,
                    AtomEnum::ATOM,
                    &targets,
                )?;
                true
            } else if req.target == a.URI_LIST {
                let data = uris.iter().map(|u| format!("{u}\r\n")).collect::<String>();
                self.conn.change_property8(
                    PropMode::REPLACE,
                    req.requestor,
                    property,
                    req.target,
                    data.as_bytes(),
                )?;
                true
            } else if req.target == a.GNOME_FILES {
                let verb = if self.move_files { "cut" } else { "copy" };
                let data = format!("{verb}\n{}", uris.join("\n"));
                self.conn.change_property8(
                    PropMode::REPLACE,
                    req.requestor,
                    property,
                    req.target,
                    data.as_bytes(),
                )?;
                true
            } else if req.target == a.UTF8_STRING || req.target == a.TEXT_PLAIN {
                let data = self
                    .paths
                    .iter()
                    .map(|p| p.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("\n");
                self.conn.change_property8(
                    PropMode::REPLACE,
                    req.requestor,
                    property,
                    req.target,
                    data.as_bytes(),
                )?;
                true
            } else {
                false
            };
            let notify = SelectionNotifyEvent {
                response_type: SELECTION_NOTIFY_EVENT,
                sequence: 0,
                time: req.time,
                requestor: req.requestor,
                selection: req.selection,
                target: req.target,
                property: if ok { property } else { NONE },
            };
            self.conn
                .send_event(false, req.requestor, EventMask::NO_EVENT, notify)?;
            self.conn.flush()?;
            Ok(())
        }
    }
}

#[cfg(all(test, unix, not(target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn file_uris_are_percent_encoded() {
        assert_eq!(
            file_uri(Path::new("/home/a/My Photo #1 é.jpg")),
            "file:///home/a/My%20Photo%20%231%20%C3%A9.jpg"
        );
    }
}

/// Manual end-to-end check against a real X server: hold mouse button 1,
/// run this, move over an XDND target and release.
/// `DND_FILE=/path cargo test dnd_manual -- --ignored`
#[cfg(all(test, unix, not(target_os = "macos")))]
#[test]
#[ignore]
fn dnd_manual() {
    let file = PathBuf::from(std::env::var("DND_FILE").unwrap());
    let active = Arc::new(AtomicBool::new(true));
    x11::start(vec![file], false, active.clone(), egui::Context::default()).unwrap();
    let start = std::time::Instant::now();
    while active.load(Ordering::Relaxed) && start.elapsed() < std::time::Duration::from_secs(20) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(!active.load(Ordering::Relaxed), "drag never finished");
}
