//! The back half of the microkernel: a single worker thread that owns the
//! `ZTEClient` and turns `Command`s into modem operations and `Event`s. All the
//! blocking HTTP happens here, never on the UI thread.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender};

use zte_control::{decode_bands, get_first_non_empty, fleet_rotate, FleetConfig, ZTEClient};

use crate::api::{Command, Event, StatusSnapshot};

/// Bands the scanner sweeps (name, LTE mask), matching the web dashboard.
const SCAN_BANDS: &[(&str, &str)] = &[
    ("Band 8 (900)", "0x0000000000000080"),
    ("Band 3 (1800)", "0x0000000000000004"),
    ("Band 7 (2600)", "0x0000000000000040"),
    ("Band 20 (800)", "0x0000000000080000"),
];

/// Seconds the WebUI login is locked for, if currently locked (else None).
fn lock_seconds(client: &ZTEClient) -> Option<u64> {
    client
        .get_cmd("login_lock_time", false)
        .ok()
        .and_then(|m| m.get("login_lock_time").and_then(|v| v.as_str()).map(|s| s.to_string()))
        .and_then(|s| s.trim().parse::<i64>().ok())
        .filter(|n| *n > 0)
        .map(|n| n as u64)
}

/// Spawn the backend worker. `repaint` wakes the egui UI after each event batch.
pub fn spawn(cmd_rx: Receiver<Command>, ev_tx: Sender<Event>, repaint: impl Fn() + Send + 'static) {
    std::thread::spawn(move || {
        let mut client = ZTEClient::new("http://192.168.8.1", "", None);
        // True only after a login with the configured password succeeds. Gates
        // auto re-login on the poll loop so a wrong/empty password is NEVER retried
        // every tick (which would trip the modem's 5-try lockout in seconds).
        let mut password_good = false;
        let log = |t: &Sender<Event>, m: String| {
            let _ = t.send(Event::Log(m));
        };

        for cmd in cmd_rx {
            match cmd {
                Command::Configure { host, password, bind_ip } => {
                    client = ZTEClient::new(&host, &password, bind_ip.as_deref());
                    if password.is_empty() {
                        password_good = false;
                        log(&ev_tx, format!(
                            "Connected to {} (read-only — enter a password for control & live IP/band)",
                            host
                        ));
                    } else if let Some(secs) = lock_seconds(&client) {
                        // Attempting a login during a lockout only extends it, so don't.
                        password_good = false;
                        let _ = ev_tx.send(Event::Error(format!(
                            "WebUI login is locked (~{}s left, too many attempts). Wait, then Connect again.",
                            secs
                        )));
                    } else {
                        // Log in exactly once, here.
                        match client.ensure_logged_in() {
                            Ok(()) => {
                                password_good = true;
                                log(&ev_tx, format!("Logged in to {}", host));
                            }
                            Err(e) => {
                                password_good = false;
                                let _ = ev_tx.send(Event::Error(format!("login failed ({}) — check the password", e)));
                            }
                        }
                    }
                    if let Ok(s) = fetch_status(&client, password_good) {
                        let _ = ev_tx.send(Event::Status(s));
                    }
                }
                // Poll failures are transient and MUST NOT be logged (would spam);
                // just skip this tick.
                Command::RefreshStatus => {
                    if let Ok(s) = fetch_status(&client, password_good) {
                        let _ = ev_tx.send(Event::Status(s));
                    }
                }
                Command::Rotate | Command::Reconnect => {
                    let _ = ev_tx.send(Event::Busy(true));
                    log(&ev_tx, "Rotating (band-hop + reconnect)…".into());
                    match client.rotate_and_reconnect() {
                        Ok(ip) => {
                            let _ = ev_tx.send(Event::RotationDone(ip));
                        }
                        Err(e) => {
                            let _ = ev_tx.send(Event::Error(e));
                        }
                    }
                    if let Ok(s) = fetch_status(&client, password_good) {
                        let _ = ev_tx.send(Event::Status(s));
                    }
                    let _ = ev_tx.send(Event::Busy(false));
                }
                Command::LockBands(bands) => {
                    let _ = ev_tx.send(Event::Busy(true));
                    match client.lock_bands(&bands) {
                        Ok(r) => log(&ev_tx, format!("Lock {:?}: {}", bands, r)),
                        Err(e) => {
                            let _ = ev_tx.send(Event::Error(e));
                        }
                    }
                    if let Ok(s) = fetch_status(&client, password_good) {
                        let _ = ev_tx.send(Event::Status(s));
                    }
                    let _ = ev_tx.send(Event::Busy(false));
                }
                Command::UnlockBands => {
                    let _ = ev_tx.send(Event::Busy(true));
                    match client.unlock_bands() {
                        Ok(r) => log(&ev_tx, format!("All bands re-enabled: {}", r)),
                        Err(e) => {
                            let _ = ev_tx.send(Event::Error(e));
                        }
                    }
                    if let Ok(s) = fetch_status(&client, password_good) {
                        let _ = ev_tx.send(Event::Status(s));
                    }
                    let _ = ev_tx.send(Event::Busy(false));
                }
                Command::ScanBands => {
                    let _ = ev_tx.send(Event::Busy(true));
                    log(&ev_tx, "Scanning bands for towers…".into());
                    for (name, mask) in SCAN_BANDS {
                        log(&ev_tx, format!("  scanning {}…", name));
                        // Clear cell lock, then force this LTE band only (2G/3G off)
                        // so the modem camps on whatever LTE cell that band offers.
                        let mut clr = HashMap::new();
                        clr.insert("lte_earfcn_lock".to_string(), "0".to_string());
                        clr.insert("lte_pci_lock".to_string(), "0".to_string());
                        let _ = client.post_cmd("LTE_LOCK_CELL_SET", clr, true);
                        let mut p = HashMap::new();
                        p.insert("is_gw_band".to_string(), "0".to_string());
                        p.insert("gw_band_mask".to_string(), "0".to_string());
                        p.insert("is_lte_band".to_string(), "1".to_string());
                        p.insert("lte_band_mask".to_string(), (*mask).to_string());
                        let _ = client.post_cmd("BAND_SELECT", p, true);
                        std::thread::sleep(std::time::Duration::from_millis(3000));
                        // Recording happens via record_tower when the kernel gets this Status.
                        if let Ok(s) = fetch_status(&client, password_good) {
                            let _ = ev_tx.send(Event::Status(s));
                        }
                    }
                    log(&ev_tx, "Restoring all bands…".into());
                    let _ = client.unlock_bands();
                    std::thread::sleep(std::time::Duration::from_millis(2000));
                    if let Ok(s) = fetch_status(&client, password_good) {
                        let _ = ev_tx.send(Event::Status(s));
                    }
                    let _ = ev_tx.send(Event::Busy(false));
                    log(&ev_tx, "Band scan complete.".into());
                }
                Command::FleetOnce(json) => {
                    let _ = ev_tx.send(Event::Busy(true));
                    match serde_json::from_str::<FleetConfig>(&json) {
                        Ok(cfg) => {
                            log(&ev_tx, "Running one make-before-break fleet cycle…".into());
                            match fleet_rotate(cfg, true) {
                                Ok(()) => log(&ev_tx, "Fleet cycle complete.".into()),
                                Err(e) => {
                                    let _ = ev_tx.send(Event::Error(e));
                                }
                            }
                        }
                        Err(e) => {
                            let _ = ev_tx.send(Event::Error(format!("bad fleet JSON: {}", e)));
                        }
                    }
                    let _ = ev_tx.send(Event::Busy(false));
                }
            }
            repaint();
        }
    });
}

fn fetch_status(client: &ZTEClient, password_good: bool) -> Result<StatusSnapshot, String> {
    // wan_ipaddr / wan_active_band / cell fields are auth-gated. Only refresh the
    // session when the password is known-good (`ensure_logged_in` no-ops while the
    // session is still valid, and re-logins succeed since the password is correct),
    // so we never retry a bad password on the poll loop.
    if password_good {
        let _ = client.ensure_logged_in();
    }
    let keys = "wa_inner_version,hardware_version,imei,network_provider,network_type,\
                network_lte_rsrp,lte_rsrp,lte_band_lock,wan_active_band,wan_ipaddr,ppp_status,\
                cell_id,network_cell_id,lte_pci,lte_earfcn,wan_active_channel";
    let m = client.get_cmd(keys, true)?;
    let g = |ks: &[&str]| get_first_non_empty(&m, ks, "").to_string();
    Ok(StatusSnapshot {
        firmware: g(&["wa_inner_version"]),
        device: g(&["hardware_version", "imei"]),
        operator: g(&["network_provider"]),
        network_type: g(&["network_type"]),
        band: g(&["wan_active_band"]),
        rsrp: g(&["network_lte_rsrp", "lte_rsrp"]),
        wan_ip: g(&["wan_ipaddr"]),
        ppp: g(&["ppp_status"]),
        bands_mask: decode_bands(get_first_non_empty(&m, &["lte_band_lock"], "0x0")),
        cell_id: g(&["cell_id", "network_cell_id"]),
        pci: g(&["lte_pci"]),
        earfcn: g(&["lte_earfcn", "wan_active_channel"]),
    })
}
