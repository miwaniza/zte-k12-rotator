use crate::kernel::{Ctx, Module};

#[derive(Default)]
pub struct ConnectionsModule;

fn d(s: &str) -> &str {
    if s.is_empty() { "—" } else { s }
}

impl Module for ConnectionsModule {
    fn title(&self) -> &'static str {
        "Connections"
    }

    fn view(&mut self, ui: &mut egui::Ui, cx: &Ctx) {
        ui.heading(format!("Connection history ({})", cx.model.history.len()));
        ui.label("Every bearer (re)connection and rotation, newest at the bottom.");
        ui.add_space(8.0);

        egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
            if cx.model.history.is_empty() {
                ui.weak("No connections recorded yet.");
                return;
            }
            egui::Grid::new("history").num_columns(6).striped(true).spacing([14.0, 4.0]).show(ui, |ui| {
                for h in ["Time (UTC)", "Event", "Operator", "Network", "Band", "WAN IP"] {
                    ui.strong(h);
                }
                ui.end_row();
                for r in &cx.model.history {
                    ui.monospace(r.ts.as_str());
                    ui.label(r.kind.as_str());
                    ui.label(d(&r.operator));
                    ui.label(d(&r.network));
                    ui.label(d(&r.band));
                    ui.monospace(d(&r.wan_ip));
                    ui.end_row();
                }
            });
        });
    }
}
