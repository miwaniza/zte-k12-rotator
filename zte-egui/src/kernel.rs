//! The microkernel. It owns the front<->back bus, the shared `AppModel`, and a
//! registry of feature `Module`s. It knows nothing about any specific module or
//! about the modem — modules talk to the backend only through `Command`/`Event`.

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use crate::api::{Command, ConnRow, Event, StatusSnapshot, Tower};

/// Shared, read-mostly UI state. The kernel is the only writer (via `apply`);
/// modules read it and emit `Command`s.
#[derive(Default)]
pub struct AppModel {
    pub status: StatusSnapshot,
    pub log: Vec<String>,
    pub busy: bool,
    pub last_error: Option<String>,
    pub towers: Vec<Tower>,
    pub history: Vec<ConnRow>,
    last_ip: String,
}

impl AppModel {
    fn apply(&mut self, ev: Event) {
        match ev {
            Event::Status(s) => {
                self.record_tower(&s);
                if !s.wan_ip.is_empty() && s.wan_ip != self.last_ip {
                    self.last_ip = s.wan_ip.clone();
                    self.history.push(ConnRow {
                        ts: now_hms(),
                        kind: "connect".into(),
                        operator: s.operator.clone(),
                        network: s.network_type.clone(),
                        band: s.band.clone(),
                        wan_ip: s.wan_ip.clone(),
                    });
                    self.cap_history();
                }
                self.status = s;
            }
            Event::Log(m) => self.push(m),
            Event::Busy(b) => self.busy = b,
            Event::RotationDone(ip) => {
                self.push(format!("Rotated → new IP {}", ip));
                self.history.push(ConnRow {
                    ts: now_hms(),
                    kind: "rotate".into(),
                    operator: self.status.operator.clone(),
                    network: self.status.network_type.clone(),
                    band: self.status.band.clone(),
                    wan_ip: ip,
                });
                self.cap_history();
            }
            Event::Error(e) => {
                self.last_error = Some(e.clone());
                self.push(format!("⚠ {}", e));
            }
        }
    }

    /// Upsert the currently-serving cell into the discovered-towers list.
    fn record_tower(&mut self, s: &StatusSnapshot) {
        let key = match s.tower_key() {
            Some(k) => k,
            None => return,
        };
        let ts = now_hms();
        if let Some(t) = self.towers.iter_mut().find(|t| t.key == key) {
            t.seen += 1;
            t.rsrp = s.rsrp.clone();
            t.band = s.band.clone();
            t.network = s.network_type.clone();
            t.last_ts = ts;
        } else {
            self.towers.push(Tower {
                key,
                cell_id: s.cell_id.clone(),
                pci: s.pci.clone(),
                earfcn: s.earfcn.clone(),
                band: s.band.clone(),
                rsrp: s.rsrp.clone(),
                network: s.network_type.clone(),
                seen: 1,
                last_ts: ts,
            });
            if self.towers.len() > 200 {
                self.towers.remove(0);
            }
        }
    }

    fn cap_history(&mut self) {
        let n = self.history.len();
        if n > 500 {
            self.history.drain(0..n - 500);
        }
    }

    fn push(&mut self, m: String) {
        // Skip consecutive duplicate lines so transient repeats don't flood the log.
        if self.log.last().map(|l| l.as_str()) == Some(m.as_str()) {
            return;
        }
        self.log.push(m);
        let n = self.log.len();
        if n > 400 {
            self.log.drain(0..n - 400);
        }
    }
}

/// Render one "Key: value" property chip for the persistent header.
fn prop(ui: &mut egui::Ui, k: &str, v: &str) {
    ui.label(egui::RichText::new(k).weak());
    ui.label(egui::RichText::new(if v.is_empty() { "—" } else { v }).strong().monospace());
    ui.separator();
}

/// Wall-clock HH:MM:SS in UTC (no external time crate needed).
fn now_hms() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{:02}:{:02}:{:02}", (secs / 3600) % 24, (secs / 60) % 60, secs % 60)
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
    // Tray is held to keep it alive; ids let us match menu events.
    _tray: Option<tray_icon::TrayIcon>,
    tray_rotate: tray_icon::menu::MenuId,
    tray_show: tray_icon::menu::MenuId,
    tray_quit: tray_icon::menu::MenuId,
}

impl Kernel {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();
        let (ev_tx, ev_rx) = mpsc::channel::<Event>();

        let ctx = cc.egui_ctx.clone();
        crate::backend::spawn(cmd_rx, ev_tx, move || ctx.request_repaint());

        // Auto-connect with saved credentials so status/towers/history populate
        // on launch without re-entering the password every time.
        let saved = crate::settings::load();
        if !saved.password.is_empty() {
            let bind_ip = {
                let b = saved.bind_ip.trim();
                if b.is_empty() { None } else { Some(b.to_string()) }
            };
            let _ = cmd_tx.send(Command::Configure {
                host: saved.host.clone(),
                password: saved.password.clone(),
                bind_ip,
            });
        }

        let (tray, tray_rotate, tray_show, tray_quit) = build_tray();

        Self {
            model: AppModel::default(),
            modules: crate::modules::all(),
            active: 0,
            tx: cmd_tx,
            rx: ev_rx,
            last_poll: Instant::now() - Duration::from_secs(10),
            _tray: tray,
            tray_rotate,
            tray_show,
            tray_quit,
        }
    }
}

/// Build the system-tray icon + menu. Returns the tray (kept alive) and the menu
/// item ids so the kernel can match tray menu events.
fn build_tray() -> (
    Option<tray_icon::TrayIcon>,
    tray_icon::menu::MenuId,
    tray_icon::menu::MenuId,
    tray_icon::menu::MenuId,
) {
    use tray_icon::menu::{Menu, MenuItem};

    let rotate = MenuItem::new("Rotate IP now", true, None);
    let show = MenuItem::new("Show window", true, None);
    let quit = MenuItem::new("Quit", true, None);
    let (rid, sid, qid) = (rotate.id().clone(), show.id().clone(), quit.id().clone());

    let menu = Menu::new();
    let _ = menu.append_items(&[&rotate, &show, &quit]);

    // Simple solid-color 32x32 tray icon (no image decoding needed).
    let mut rgba = Vec::with_capacity(32 * 32 * 4);
    for _ in 0..(32 * 32) {
        rgba.extend_from_slice(&[110, 64, 201, 255]);
    }
    let icon = tray_icon::Icon::from_rgba(rgba, 32, 32).ok();

    let mut builder = tray_icon::TrayIconBuilder::new()
        .with_tooltip("ZTE Control")
        .with_menu(Box::new(menu));
    if let Some(ic) = icon {
        builder = builder.with_icon(ic);
    }
    (builder.build().ok(), rid, sid, qid)
}

impl eframe::App for Kernel {
    fn update(&mut self, ctx: &egui::Context, _f: &mut eframe::Frame) {
        // Drain backend events into the shared model.
        while let Ok(ev) = self.rx.try_recv() {
            self.model.apply(ev);
        }

        // Handle system-tray menu clicks.
        if let Ok(ev) = tray_icon::menu::MenuEvent::receiver().try_recv() {
            if ev.id == self.tray_rotate {
                let _ = self.tx.send(Command::Rotate);
            } else if ev.id == self.tray_show {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            } else if ev.id == self.tray_quit {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        // Periodic read-only status poll.
        if self.last_poll.elapsed() >= Duration::from_secs(3) {
            let _ = self.tx.send(Command::RefreshStatus);
            self.last_poll = Instant::now();
        }

        // Persistent header: connection status + Rotate button + live properties,
        // visible on every tab.
        egui::TopBottomPanel::top("bar").show(ctx, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading("ZTE Control");
                ui.separator();
                ui.label(if self.model.status.connected() { "🟢" } else { "🔴" });
                ui.add_enabled_ui(!self.model.busy, |ui| {
                    if ui.button("🔄  Rotate IP").clicked() {
                        let _ = self.tx.send(Command::Rotate);
                    }
                });
                if self.model.busy {
                    ui.spinner();
                    ui.label("working…");
                }
            });
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                let s = &self.model.status;
                prop(ui, "Operator", &s.operator);
                prop(ui, "Network", &s.network_type);
                prop(ui, "Band", &s.band);
                prop(ui, "IP", &s.wan_ip);
                prop(ui, "RSRP", &s.rsrp);
                prop(ui, "PPP", &s.ppp);
            });
            ui.add_space(6.0);
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
