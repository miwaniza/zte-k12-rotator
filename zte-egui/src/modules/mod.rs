//! Feature modules registered with the kernel. Each is self-contained: it renders
//! its own egui view and emits `Command`s; none touch the modem directly.

mod connection;
mod connections;
mod fleet;
mod log;
mod rotation;
mod status;
mod towers;

use crate::kernel::Module;

/// The modules the kernel hosts, in tab order.
pub fn all() -> Vec<Box<dyn Module>> {
    vec![
        Box::new(connection::ConnectionModule::default()),
        Box::new(status::StatusModule),
        Box::new(rotation::RotationModule),
        Box::new(towers::TowersModule),
        Box::new(connections::ConnectionsModule),
        Box::new(fleet::FleetModule::default()),
        Box::new(log::LogModule),
    ]
}
