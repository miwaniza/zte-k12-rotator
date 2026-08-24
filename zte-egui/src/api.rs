//! The front <-> back API. This is the ONLY coupling between the UI (front) and
//! the modem worker (back): the UI sends `Command`s, the backend replies with
//! `Event`s. Nothing in the UI touches the modem directly.

/// UI -> backend requests.
#[derive(Debug, Clone)]
pub enum Command {
    /// Point the client at a modem.
    Configure {
        host: String,
        password: String,
        bind_ip: Option<String>,
    },
    /// Read-only status refresh.
    RefreshStatus,
    /// Band-hop + reconnect for a new IP (self-healing).
    Rotate,
    /// Drop and re-establish the bearer.
    Reconnect,
    /// Lock to specific LTE bands (e.g. ["B3"], or ["ALL"]).
    LockBands(Vec<String>),
    /// Re-enable all bands (2G/3G + LTE) and clear locks.
    UnlockBands,
    /// Run ONE make-before-break fleet cycle from a fleet.json string.
    FleetOnce(String),
}

/// backend -> UI notifications.
#[derive(Debug, Clone)]
pub enum Event {
    Status(StatusSnapshot),
    Log(String),
    Busy(bool),
    RotationDone(String),
    Error(String),
}

/// A flattened, UI-friendly view of the modem status.
#[derive(Debug, Clone, Default)]
pub struct StatusSnapshot {
    pub device: String,
    pub firmware: String,
    pub operator: String,
    pub network_type: String,
    pub band: String,
    pub rsrp: String,
    pub wan_ip: String,
    pub ppp: String,
    pub bands_mask: String,
}

impl StatusSnapshot {
    pub fn connected(&self) -> bool {
        self.ppp == "ppp_connected"
    }
}
