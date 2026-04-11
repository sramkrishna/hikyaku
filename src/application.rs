// MxApplication — our AdwApplication subclass.
//
// In GTK/libadwaita, the Application object owns the main loop and manages
// windows. We subclass it to hook into lifecycle signals like "activate"
// (called when the app starts or is raised).

mod imp {
    use adw::subclass::prelude::*;
    use gtk::gio;
    use gtk::prelude::*;
    use gtk::glib;

    use crate::config;
    use crate::matrix;
    use crate::widgets::MxWindow;

    use async_channel::Receiver;
    use std::cell::OnceCell;

    pub struct MxApplication {
        pub event_rx: OnceCell<Receiver<matrix::MatrixEvent>>,
    }

    impl Default for MxApplication {
        fn default() -> Self {
            Self {
                event_rx: OnceCell::new(),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MxApplication {
        const NAME: &'static str = "MxApplication";
        type Type = super::MxApplication;
        // We're extending AdwApplication, which itself extends GtkApplication.
        type ParentType = adw::Application;
    }

    impl ObjectImpl for MxApplication {}

    // GtkApplication's "activate" signal fires on startup. This is where we
    // create the channels, spawn the Matrix tokio thread, and open the window.
    impl ApplicationImpl for MxApplication {
        fn activate(&self) {
            self.parent_activate();

            let app = self.obj();

            // If we already have a window, just present it (handles re-activation).
            if let Some(window) = app.active_window() {
                window.present();
                return;
            }

            // Create the bidirectional async channels.
            // Events flow: Matrix thread → GTK main loop
            // Commands flow: GTK main loop → Matrix thread
            let (event_tx, event_rx) = async_channel::unbounded::<matrix::MatrixEvent>();
            let (command_tx, command_rx) = async_channel::unbounded::<matrix::MatrixCommand>();

            // Spawn the background tokio thread that runs matrix-sdk.
            // Shutdown is initiated by sending MatrixCommand::Shutdown — all
            // commands before it are guaranteed to run first (no race condition).
            let timeline_cache = matrix::spawn_matrix_thread(event_tx, command_rx);

            // Store event channel (command_tx lives only in the window).
            let _ = self.event_rx.set(event_rx.clone());

            // On app shutdown: flush pending read receipt then send Shutdown.
            // The tokio command loop processes commands in FIFO order, so
            // MarkRead is guaranteed to complete before Shutdown terminates the loop.
            app.connect_shutdown(|_app| {
                tracing::info!("Application shutting down");
                crate::intelligence::ollama_manager::stop();
                // Failsafe: force exit after 5 seconds if the sync loop hangs.
                std::thread::spawn(|| {
                    std::thread::sleep(std::time::Duration::from_secs(5));
                    tracing::warn!("Force exiting — sync did not stop in time");
                    std::process::exit(0);
                });
            });

            // Register keyboard accelerators (GNOME HIG).
            app.set_accels_for_action("win.preferences",  &["<Control>comma"]);
            app.set_accels_for_action("win.shortcuts",    &["<Control>question"]);
            app.set_accels_for_action("win.join-room",    &["<Control><Shift>j"]);
            app.set_accels_for_action("win.prev-room",    &["<Alt>Up"]);
            app.set_accels_for_action("win.next-room",    &["<Alt>Down"]);

            // For dev builds (cargo run), register the local icon directory so
            // GTK can find the app icon without a system install.
            if let Some(display) = gtk::gdk::Display::default() {
                let theme = gtk::IconTheme::for_display(&display);
                // Prefer exe-relative path (handles both cargo run and installed).
                if let Ok(exe) = std::env::current_exe() {
                    if let Some(dir) = exe.parent() {
                        let p = dir.join("../../../data/icons");
                        if p.exists() { theme.add_search_path(&p); }
                    }
                }
                // Fallback: working-directory relative (cargo run from workspace root).
                if let Ok(cwd) = std::env::current_dir() {
                    let p = cwd.join("data/icons");
                    if p.exists() { theme.add_search_path(&p); }
                }
            }

            // Create and present the main window.
            let window = MxWindow::new(&app, event_rx, command_tx, timeline_cache);
            window.set_title(Some(config::APP_NAME));
            window.set_default_size(1000, 700);

            // On close: flush pending read receipt, send Shutdown, then quit the app.
            // We do this in connect_close_request (not connect_shutdown) because the
            // window is still alive here — connect_shutdown fires after it's destroyed.
            let app_weak = app.downgrade();
            window.connect_close_request(move |win| {
                let wimp = win.imp();
                tracing::info!("close_request: window closing");
                if let Some(cmd_tx) = wimp.command_tx.get() {
                    // If the 15-second read-receipt timer hasn't fired yet, cancel
                    // it and send MarkRead now so it reaches the server before shutdown.
                    match wimp.read_timer.borrow_mut().take() {
                        Some((src, fired, rid)) if !fired.get() => {
                            tracing::info!("close_request: timer unfired, sending MarkRead for {rid}");
                            src.remove();
                            let _ = cmd_tx.try_send(
                                crate::matrix::MatrixCommand::MarkRead { room_id: rid }
                            );
                        }
                        Some((_, _, rid)) => {
                            tracing::info!("close_request: timer already fired for {rid}, no MarkRead needed");
                        }
                        None => {
                            tracing::info!("close_request: no active read timer");
                        }
                    }
                    tracing::info!("close_request: sending Shutdown");
                    let _ = cmd_tx.try_send(crate::matrix::MatrixCommand::Shutdown);
                } else {
                    tracing::warn!("close_request: no command_tx — MarkRead/Shutdown not sent");
                }
                if let Some(app) = app_weak.upgrade() {
                    app.quit();
                }
                // Stop: app.quit() already tears down the window — returning
                // Proceed would cause a second destroy and crash.
                glib::Propagation::Stop
            });
            window.present();
        }

        // Handle matrix: URI scheme (invoked by the browser or other apps).
        fn open(&self, files: &[gio::File], _hint: &str) {
            // Ensure the window is up.
            self.activate();
            let app = self.obj();
            let Some(window) = app.active_window() else { return };
            let Some(win) = window.downcast_ref::<crate::widgets::MxWindow>() else { return };
            for file in files {
                let uri = file.uri();
                if let Some(matrix_id) = super::parse_matrix_uri(uri.as_str()) {
                    win.handle_matrix_link(&matrix_id);
                }
            }
        }
    }

    impl GtkApplicationImpl for MxApplication {}
    impl AdwApplicationImpl for MxApplication {}
}

use gtk::glib;
use gtk::gio;

/// Extract a Matrix room ID or alias from a matrix: URI or https://matrix.to link.
///
/// Handles:
///   - `matrix:r/alias/server` → `#alias:server`
///   - `matrix:roomid/!id/server` → `!id:server`
///   - `https://matrix.to/#/!roomid:server` → `!roomid:server`
///   - `https://matrix.to/#/#alias:server` → `#alias:server`
fn parse_matrix_uri(uri: &str) -> Option<String> {
    // matrix: URI scheme (MSC2312).
    if let Some(rest) = uri.strip_prefix("matrix:r/") {
        let alias = rest.split('?').next().unwrap_or(rest);
        return Some(format!("#{}", alias.replacen('/', ":", 1)));
    }
    if let Some(rest) = uri.strip_prefix("matrix:roomid/") {
        let id = rest.split('?').next().unwrap_or(rest);
        return Some(format!("!{}", id.replacen('/', ":", 1)));
    }
    // https://matrix.to/#/ links (opened via xdg-open when registered as handler).
    if let Some(rest) = uri.strip_prefix("https://matrix.to/#/") {
        let id = rest.split('?').next().unwrap_or(rest);
        // Minimal percent-decode for %21 (!) and %23 (#).
        let id = percent_decode(id);
        if id.starts_with('!') || id.starts_with('#') {
            return Some(id);
        }
    }
    None
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(hex) = u8::from_str_radix(
                std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""),
                16,
            ) {
                out.push(hex);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

glib::wrapper! {
    pub struct MxApplication(ObjectSubclass<imp::MxApplication>)
        @extends adw::Application, gtk::Application, gio::Application,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl MxApplication {
    pub fn new() -> Self {
        glib::Object::builder()
            .property("application-id", crate::config::APP_ID)
            .property("flags", gio::ApplicationFlags::HANDLES_OPEN)
            .build()
    }
}
