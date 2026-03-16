// MxApplication — our AdwApplication subclass.
//
// In GTK/libadwaita, the Application object owns the main loop and manages
// windows. We subclass it to hook into lifecycle signals like "activate"
// (called when the app starts or is raised).

mod imp {
    use adw::subclass::prelude::*;
    use gtk::prelude::*;
    use gtk::glib;

    use crate::config;
    use crate::matrix;
    use crate::widgets::MxWindow;

    use async_channel::{Receiver, Sender};
    use std::cell::OnceCell;

    pub struct MxApplication {
        pub event_rx: OnceCell<Receiver<matrix::MatrixEvent>>,
        pub command_tx: OnceCell<Sender<matrix::MatrixCommand>>,
        pub shutdown_tx: OnceCell<tokio::sync::watch::Sender<bool>>,
    }

    impl Default for MxApplication {
        fn default() -> Self {
            Self {
                event_rx: OnceCell::new(),
                command_tx: OnceCell::new(),
                shutdown_tx: OnceCell::new(),
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
            let shutdown_tx = matrix::spawn_matrix_thread(event_tx, command_rx);

            // Store channels and shutdown handle.
            let _ = self.event_rx.set(event_rx.clone());
            let _ = self.command_tx.set(command_tx.clone());
            let _ = self.shutdown_tx.set(shutdown_tx);

            // Signal the Matrix thread on app shutdown (window close or Ctrl-C).
            app.connect_shutdown(|app| {
                tracing::info!("Application shutting down, signaling Matrix thread");
                let imp = app.imp();
                if let Some(tx) = imp.shutdown_tx.get() {
                    let _ = tx.send(true);
                }
                // Failsafe: force exit after 3 seconds if sync hangs.
                std::thread::spawn(|| {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    tracing::warn!("Force exiting — sync did not stop in time");
                    std::process::exit(0);
                });
            });

            // Create and present the main window.
            let window = MxWindow::new(&app, event_rx, command_tx);
            window.set_title(Some(config::APP_NAME));
            window.set_default_size(1000, 700);

            // Ensure closing the window quits the app.
            let app_weak = app.downgrade();
            window.connect_close_request(move |_| {
                if let Some(app) = app_weak.upgrade() {
                    app.quit();
                }
                glib::Propagation::Proceed
            });
            window.present();
        }
    }

    impl GtkApplicationImpl for MxApplication {}
    impl AdwApplicationImpl for MxApplication {}
}

use gtk::glib;

glib::wrapper! {
    pub struct MxApplication(ObjectSubclass<imp::MxApplication>)
        @extends adw::Application, gtk::Application, gio::Application,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl MxApplication {
    pub fn new() -> Self {
        glib::Object::builder()
            .property("application-id", crate::config::APP_ID)
            .build()
    }
}
