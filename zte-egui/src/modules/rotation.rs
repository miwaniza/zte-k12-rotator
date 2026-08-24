use crate::api::Command;
use crate::kernel::{Ctx, Module};

#[derive(Default)]
pub struct RotationModule;

impl Module for RotationModule {
    fn title(&self) -> &'static str {
        "Rotate"
    }

    fn view(&mut self, ui: &mut egui::Ui, cx: &Ctx) {
        ui.heading("IP rotation & bands");
        ui.add_space(8.0);

        ui.add_enabled_ui(!cx.model.busy, |ui| {
            if ui
                .add_sized([240.0, 44.0], egui::Button::new("🔄  Rotate IP now"))
                .clicked()
            {
                cx.send(Command::Rotate);
            }

            ui.add_space(14.0);
            ui.label("Lock to LTE band:");
            ui.horizontal(|ui| {
                for b in ["B3", "B7", "B8", "B20", "ALL"] {
                    if ui.button(b).clicked() {
                        cx.send(Command::LockBands(vec![b.to_string()]));
                    }
                }
            });

            ui.add_space(10.0);
            if ui.button("Unlock all bands  (recover NO_SERVICE)").clicked() {
                cx.send(Command::UnlockBands);
            }
            ui.add_space(4.0);
            if ui.button("Reconnect bearer").clicked() {
                cx.send(Command::Reconnect);
            }
        });

        if cx.model.busy {
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("working…");
            });
        }
    }
}
