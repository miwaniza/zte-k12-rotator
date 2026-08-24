//! The microkernel. It owns the front<->back bus, the shared `AppModel`, and a
//! registry of feature `Module`s. It knows nothing about any specific module or
//! about the modem — modules talk to the backend only through `Command`/`Event`.

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use crate::api::{Command, Event, StatusSnapshot};

/// Shared, read-mostly UI state. The kernel is the only writer (via `apply`);
/// modules read it and emit `Command`s.
#[derive(Default)]
pub struct AppModel {
    pub status: StatusSnapshot,
    pub log: Vec<String>,
    pub busy: bool,
    pub last_error: Option<String>,
}

impl AppModel {
    fn apply(&mut self, ev: Event) {
        match ev {
            Event::Status(s) => self.status = s,
            Event::Log(m) => self.push(m),
            Event::Busy(b) => self.busy = b,
            Event::RotationDone(ip) => self.push(format!("Rotated → new IP {}", ip)),
            Event::Error(e) => {
                self.last_error = Some(e.clone());
                self.push(format!("⚠ {}", e));
            }
        }
    }
    fn push(&mut self, m: String) {
        self.log.push(m);
        let n = self.log.len();
        if n > 400 {
            self.log.drain(0..n - 400);
        }
    }
}

/// Handed to a module on each frame: read the model, send commands to the back.
pub struct Ctx<'a> {
    pub model: &'a AppModel,
    tx: &'a Sender<Command>,
}
impl Ctx<'_> {
    pub fn send(&self, c: Command) {
        let _ = self.tx.send(c);
    }
}

/// A self-contained feature. Modules are decoupled: they render + emit commands.
pub trait Module {
    fn title(&self) -> &'static str;
    fn view(&mut self, ui: &mut egui::Ui, cx: &Ctx);
}

pub struct Kernel {
    model: AppModel,
    modules: Vec<Box<dyn Module>>,
    active: usize,
    tx: Sender<Command>,
    rx: Receiver<Event>,
    last_poll: Instant,
}

impl Kernel {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();
        let (ev_tx, ev_rx) = mpsc::channel::<Event>();

        let ctx = cc.egui_ctx.clone();
        crate::backend::spawn(cmd_rx, ev_tx, move || ctx.request_repaint());

        Self {
            model: AppModel::default(),
            modules: crate::modules::all(),
            active: 0,
            tx: cmd_tx,
            rx: ev_rx,
            last_poll: Instant::now() - Duration::from_secs(10),
        }
    }
}

impl eframe::App for Kernel {
    fn update(&mut self, ctx: &egui::Context, _f: &mut eframe::Frame) {
        // Drain backend events into the shared model.
        while let Ok(ev) = self.rx.try_recv() {
            self.model.apply(ev);
        }
        // Periodic read-only status poll.
        if self.last_poll.elapsed() >= Duration::from_secs(3) {
            let _ = self.tx.send(Command::RefreshStatus);
            self.last_poll = Instant::now();
        }

        egui::TopBottomPanel::top("bar").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading("ZTE Control");
                ui.separator();
                let s = &self.model.status;
                if s.connected() {
                    ui.label(format!("🟢 {} · {} · {}", s.operator, s.network_type, s.wan_ip));
                } else {
                    let t = if s.network_type.is_empty() { "not connected".to_string() } else { s.network_type.clone() };
                    ui.label(format!("🔴 {}", t));
                }
                if self.model.busy {
                    ui.spinner();
                }
            });
            ui.add_space(4.0);
        });

        egui::SidePanel::left("nav").resizable(false).exact_width(140.0).show(ctx, |ui| {
            ui.add_space(8.0);
            let mut next = self.active;
            for (i, m) in self.modules.iter().enumerate() {
                if ui.selectable_label(self.active == i, m.title()).clicked() {
                    next = i;
                }
            }
            self.active = next;
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let Kernel { model, modules, tx, active, .. } = self;
            let cx = Ctx { model, tx };
            modules[*active].view(ui, &cx);
        });

        // Keep ticking so the periodic poll fires even without user input.
        ctx.request_repaint_after(Duration::from_millis(1000));
    }
}
