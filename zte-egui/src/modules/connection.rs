use crate::api::Command;
use crate::kernel::{Ctx, Module};

pub struct ConnectionModule {
    host: String,
    password: String,
    bind_ip: String,
    show_pw: bool,
}

impl Default for ConnectionModule {
    fn default() -> Self {
        let s = crate::settings::load();
        Self {
            host: s.host,
            password: s.password,
            bind_ip: s.bind_ip,
            show_pw: false,
        }
    }
}

impl Module for ConnectionModule {
    fn title(&self) -> &'static str {
        "Connection"
    }

    fn view(&mut self, ui: &mut egui::Ui, cx: &Ctx) {
        ui.heading("Connection");
        ui.add_space(8.0);
        egui::Grid::new("conn").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("Host");
            ui.text_edit_singleline(&mut self.host);
            ui.end_row();
            ui.label("Password");
            ui.add(egui::TextEdit::singleline(&mut self.password).password(!self.show_pw));
            ui.end_row();
            ui.label("Bind IP (opt.)");
            ui.text_edit_singleline(&mut self.bind_ip);
            ui.end_row();
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.show_pw, "show");
            if ui.button("Connect").clicked() {
                let host = self.host.trim().to_string();
                let bind_ip_raw = self.bind_ip.trim().to_string();
                // Remember for next launch (auto-connect).
                crate::settings::save(&crate::settings::ConnSettings {
                    host: host.clone(),
                    password: self.password.clone(),
                    bind_ip: bind_ip_raw.clone(),
                });
                let bind_ip = if bind_ip_raw.is_empty() { None } else { Some(bind_ip_raw) };
                cx.send(Command::Configure {
                    host,
                    password: self.password.clone(),
                    bind_ip,
                });
            }
        });
        ui.add_space(8.0);
        ui.label("Both the K12 and the MF920U are supported — the core adapts the");
        ui.label("login/AD scheme to the firmware automatically.");
    }
}
