//! The back half of the microkernel: a single worker thread that owns the
//! `ZTEClient` and turns `Command`s into modem operations and `Event`s. All the
//! blocking HTTP happens here, never on the UI thread.

use std::sync::mpsc::{Receiver, Sender};

use zte_control::{decode_bands, get_first_non_empty, fleet_rotate, FleetConfig, ZTEClient};

use crate::api::{Command, Event, StatusSnapshot};

/// Spawn the backend worker. `repaint` wakes the egui UI after each event batch.
pub fn spawn(cmd_rx: Receiver<Command>, ev_tx: Sender<Event>, repaint: impl Fn() + Send + 'static) {
    std::thread::spawn(move || {
        let mut client = ZTEClient::new("http://192.168.8.1", "", None);
        let log = |t: &Sender<Event>, m: String| {
            let _ = t.send(Event::Log(m));
        };

        for cmd in cmd_rx {
            match cmd {
                Command::Configure { host, password, bind_ip } => {
                    client = ZTEClient::new(&host, &password, bind_ip.as_deref());
                    log(&ev_tx, format!("Connected to {}", host));
                    if let Ok(s) = fetch_status(&client) {
                        let _ = ev_tx.send(Event::Status(s));
                    }
                }
                Command::RefreshStatus => match fetch_status(&client) {
                    Ok(s) => {
                        let _ = ev_tx.send(Event::Status(s));
                    }
                    Err(e) => {
                        let _ = ev_tx.send(Event::Error(e));
                    }
                },
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
                    if let Ok(s) = fetch_status(&client) {
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
                    if let Ok(s) = fetch_status(&client) {
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
                    if let Ok(s) = fetch_status(&client) {
                        let _ = ev_tx.send(Event::Status(s));
                    }
                    let _ = ev_tx.send(Event::Busy(false));
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

fn fetch_status(client: &ZTEClient) -> Result<StatusSnapshot, String> {
    let keys = "wa_inner_version,hardware_version,imei,network_provider,network_type,\
                network_lte_rsrp,lte_rsrp,lte_band_lock,wan_active_band,wan_ipaddr,ppp_status";
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
    })
}
