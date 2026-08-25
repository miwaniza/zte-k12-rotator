use crate::kernel::{Ctx, Module};

#[derive(Default)]
pub struct LogModule;

impl Module for LogModule {
    fn title(&self) -> &'static str {
        "Log"
    }

    fn view(&mut self, ui: &mut egui::Ui, cx: &Ctx) {
        ui.heading("Log");
        ui.add_space(6.0);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                if cx.model.log.is_empty() {
                    ui.weak("No activity yet.");
                }
                for line in &cx.model.log {
                    ui.monospace(line);
                }
            });
    }
}
