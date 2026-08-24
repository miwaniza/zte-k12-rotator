//! Feature modules registered with the kernel. Each is self-contained: it renders
//! its own egui view and emits `Command`s; none touch the modem directly.

mod connection;
mod fleet;
mod log;
mod rotation;
mod status;

use crate::kernel::Module;

/// The modules the kernel hosts, in tab order.
pub fn all() -> Vec<Box<dyn Module>> {
    vec![
        Box::new(connection::ConnectionModule::default()),
        Box::new(status::StatusModule::default()),
        Box::new(rotation::RotationModule::default()),
        Box::new(fleet::FleetModule::default()),
        Box::new(log::LogModule::default()),
    ]
}
