use crate::api::Command;
use crate::kernel::{Ctx, Module};

#[derive(Default)]
pub struct StatusModule;

impl Module for StatusModule {
    fn title(&self) -> &'static str {
        "Status"
    }

    fn view(&mut self, ui: &mut egui::Ui, cx: &Ctx) {
        ui.horizontal(|ui| {
            ui.heading("Modem status");
            if ui.button("↻ Refresh").clicked() {
                cx.send(Command::RefreshStatus);
            }
        });
        ui.add_space(8.0);

        let s = &cx.model.status;
        egui::Grid::new("stat").num_columns(2).spacing([16.0, 6.0]).striped(true).show(ui, |ui| {
            let row = |ui: &mut egui::Ui, k: &str, v: &str| {
                ui.label(k);
                ui.monospace(if v.is_empty() { "—" } else { v });
                ui.end_row();
            };
            row(ui, "Firmware", &s.firmware);
            row(ui, "Device", &s.device);
            row(ui, "Operator", &s.operator);
            row(ui, "Network", &s.network_type);
            row(ui, "Active band", &s.band);
            row(ui, "RSRP", &s.rsrp);
            row(ui, "WAN IP", &s.wan_ip);
            row(ui, "PPP", &s.ppp);
            row(ui, "Allowed bands", &s.bands_mask);
        });
    }
}
