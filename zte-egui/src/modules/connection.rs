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
        Self {
            host: "http://192.168.8.1".to_string(),
            password: String::new(),
            bind_ip: String::new(),
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
                let bind_ip = {
                    let b = self.bind_ip.trim();
                    if b.is_empty() { None } else { Some(b.to_string()) }
                };
                cx.send(Command::Configure {
                    host: self.host.trim().to_string(),
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
