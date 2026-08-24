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
        ui.heading(format!("Discovered towers ({})", cx.model.towers.len()));
        ui.label("Serving cells the modem has camped on this session (auto-discovered from status).");
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
