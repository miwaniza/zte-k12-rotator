//! The front <-> back API. This is the ONLY coupling between the UI (front) and
//! the modem worker (back): the UI sends `Command`s, the backend replies with
//! `Event`s. Nothing in the UI touches the modem directly.

pub use zte_control::RotationOutcome;

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
    /// Lock each LTE band in turn and record the serving cell on each, to
    /// discover towers per band; restores all bands when done.
    ScanBands,
    /// Run ONE make-before-break fleet cycle from a fleet.json string.
    FleetOnce(String),
}

/// backend -> UI notifications.
#[derive(Debug, Clone)]
pub enum Event {
    Status(Box<StatusSnapshot>),
    Log(String),
    Busy(bool),
    /// A rotation finished. The outcome says whether the public address is known
    /// to have changed -- "the bearer came back" is not the same claim.
    RotationDone(RotationOutcome),
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
    pub cell_id: String,
    pub pci: String,
    pub earfcn: String,
}

impl StatusSnapshot {
    pub fn connected(&self) -> bool {
        self.ppp == "ppp_connected"
    }
    /// Stable identity of the serving cell/tower, if any is reported.
    pub fn tower_key(&self) -> Option<String> {
        let is_empty_or_zero = |s: &str| {
            let t = s.trim();
            t.is_empty() || t == "0" || t == "None" || t == "--"
        };
        if is_empty_or_zero(&self.cell_id) && is_empty_or_zero(&self.pci) {
            None
        } else {
            Some(format!("{}|{}|{}", self.cell_id, self.pci, self.earfcn))
        }
    }
}

/// A discovered serving cell ("tower"), accumulated as the modem camps on cells.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tower {
    pub key: String,
    pub cell_id: String,
    pub pci: String,
    pub earfcn: String,
    pub band: String,
    pub rsrp: String,
    pub network: String,
    pub seen: u32,
    pub last_ts: String,
}

/// One row of the connection history table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnRow {
    pub ts: String,
    pub kind: String,
    pub operator: String,
    pub network: String,
    pub band: String,
    pub wan_ip: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_snapshot_connected() {
        let mut st = StatusSnapshot::default();
        assert!(!st.connected());
        st.ppp = "ppp_connected".to_string();
        assert!(st.connected());
    }

    #[test]
    fn test_status_snapshot_tower_key() {
        let mut st = StatusSnapshot::default();
        assert_eq!(st.tower_key(), None);

        st.cell_id = "0".to_string();
        st.pci = "0".to_string();
        assert_eq!(st.tower_key(), None);

        st.cell_id = "4d82883".to_string();
        st.pci = "120".to_string();
        st.earfcn = "1650".to_string();
        assert_eq!(st.tower_key(), Some("4d82883|120|1650".to_string()));
    }
}
