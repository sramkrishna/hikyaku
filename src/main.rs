// Matx — a Matrix client built with Rust + libadwaita.
//
// Entry point: initialize logging, then hand off to GTK's application
// lifecycle. GTK takes over the main thread; the Matrix SDK runs on a
// separate tokio thread spawned during `activate`.

mod application;
mod config;
mod intelligence;
mod matrix;
mod models;
mod widgets;

use gtk::prelude::*;

fn main() {
    // Initialize structured logging. RUST_LOG=matx=debug cargo run
    // will show our debug output.
    tracing_subscriber::fmt::init();

    // Create and run the GTK application. `run()` blocks until the
    // user closes the window. It handles argc/argv for us.
    let app = application::MxApplication::new();
    app.run();
}
