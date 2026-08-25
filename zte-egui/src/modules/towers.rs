use crate::api::Command;
use crate::kernel::{Ctx, Module};

#[derive(Default)]
pub struct TowersModule;

fn d(s: &str) -> &str {
    if s.is_empty() { "—" } else { s }
}

impl Module for TowersModule {
    fn title(&self) -> &'static str {
        "Towers"
    }

    fn view(&mut self, ui: &mut egui::Ui, cx: &Ctx) {
        ui.horizontal(|ui| {
            ui.heading(format!("Discovered towers ({})", cx.model.towers.len()));
            ui.add_enabled_ui(!cx.model.busy, |ui| {
                if ui.button("🔍  Scan bands").clicked() {
                    cx.send(Command::ScanBands);
                }
            });
            if cx.model.busy {
                ui.spinner();
            }
        });
        ui.label("Passively records the current serving cell; 'Scan bands' locks each LTE band in");
        ui.label("turn to discover a tower per band (needs a login). Restores all bands after.");
        ui.add_space(8.0);

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if cx.model.towers.is_empty() {
                ui.weak("No towers observed yet — connect the modem and wait for a status poll.");
                return;
            }
            egui::Grid::new("towers").num_columns(7).striped(true).spacing([14.0, 4.0]).show(ui, |ui| {
                for h in ["Cell ID", "PCI", "EARFCN", "Band", "RSRP", "Net", "Seen"] {
                    ui.strong(h);
                }
                ui.end_row();
                for t in &cx.model.towers {
                    ui.monospace(d(&t.cell_id));
                    ui.monospace(d(&t.pci));
                    ui.monospace(d(&t.earfcn));
                    ui.label(d(&t.band));
                    ui.monospace(d(&t.rsrp));
                    ui.label(d(&t.network));
                    ui.label(t.seen.to_string());
                    ui.end_row();
                }
            });
        });
    }
}
