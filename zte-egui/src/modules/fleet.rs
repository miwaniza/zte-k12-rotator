use crate::api::Command;
use crate::kernel::{Ctx, Module};

const DEFAULT_FLEET: &str = r#"{
  "dwell_seconds": 90,
  "solid_timeout_seconds": 60,
  "probe_url": "http://cp.cloudflare.com/generate_204",
  "modems": [
    { "name": "k12",    "host": "http://192.168.0.1", "password": "CHANGE_ME",
      "bind_ip": "192.168.0.100", "iface_index": 21, "gateway": "192.168.0.1" },
    { "name": "mf920u", "host": "http://192.168.8.1", "password": "CHANGE_ME",
      "bind_ip": "192.168.8.178", "iface_index": 44, "gateway": "192.168.8.1" }
  ]
}"#;

pub struct FleetModule {
    json: String,
}

impl Default for FleetModule {
    fn default() -> Self {
        Self { json: DEFAULT_FLEET.to_string() }
    }
}

impl Module for FleetModule {
    fn title(&self) -> &'static str {
        "Fleet"
    }

    fn view(&mut self, ui: &mut egui::Ui, cx: &Ctx) {
        ui.heading("Make-before-break fleet");
        ui.label("Runs ONE ping-pong cycle: rotate the standby modem, wait until it is");
        ui.label("solid, then swap the active default route to it.");
        ui.add_space(8.0);

        egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(&mut self.json)
                    .code_editor()
                    .desired_rows(14)
                    .desired_width(f32::INFINITY),
            );
        });

        ui.add_space(8.0);
        ui.add_enabled_ui(!cx.model.busy, |ui| {
            if ui.button("▶  Run one cycle").clicked() {
                cx.send(Command::FleetOnce(self.json.clone()));
            }
        });
        ui.add_space(6.0);
        ui.label("Requires 2 modems on distinct subnets and routing privileges (admin).");
    }
}
