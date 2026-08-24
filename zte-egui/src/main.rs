//! Native (egui) desktop UI for ZTE modem control.
//!
//! Microkernel architecture:
//!   * `api`     — the front<->back contract (Command / Event / StatusSnapshot).
//!   * `backend` — a worker thread owning the ZTEClient; runs all blocking modem
//!                 I/O and emits events, so the UI thread never freezes.
//!   * `kernel`  — hosts the bus, the shared AppModel, and a registry of modules.
//!   * `modules` — self-contained features (connection, status, rotation, fleet,
//!                 log) that render + emit commands, decoupled via the api.
//!
//! No webview, no HTTP proxy — the UI talks to the shared `zte_control` core lib
//! directly through the command bus.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod api;
mod backend;
mod kernel;
mod modules;
mod settings;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1000.0, 720.0])
            .with_min_inner_size([760.0, 520.0])
            .with_title("ZTE Control — Modem Rotator"),
        ..Default::default()
    };

    eframe::run_native(
        "ZTE Control",
        options,
        Box::new(|cc| Ok(Box::new(kernel::Kernel::new(cc)))),
    )
}
