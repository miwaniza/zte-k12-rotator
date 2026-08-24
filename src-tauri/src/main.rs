// Native desktop UI for ZTE modem control. Reuses the zte-control core library
// (ZTEClient: adaptive auth, band/bearer control, rotation) directly — no HTTP
// proxy, no duplicated auth in JS. The webview calls these commands via invoke().
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, State};
use zte_control::ZTEClient;

/// The current modem client, rebuilt whenever the UI calls `configure`.
struct AppState {
    client: Mutex<ZTEClient>,
}

impl AppState {
    /// Clone out the client so the (possibly slow) HTTP call runs without holding
    /// the lock. Clones share the session cookie via an Arc, so login persists.
    fn client(&self) -> Result<ZTEClient, String> {
        self.client
            .lock()
            .map(|c| c.clone())
            .map_err(|e| e.to_string())
    }
}

fn to_value(m: HashMap<String, Value>) -> Value {
    serde_json::to_value(m).unwrap_or(Value::Null)
}

/// Point the app at a modem (host / password / optional source-bind IP).
#[tauri::command]
fn configure(
    state: State<AppState>,
    host: String,
    password: String,
    bind_ip: Option<String>,
) -> Result<(), String> {
    let bind = bind_ip.filter(|s| !s.trim().is_empty());
    let client = ZTEClient::new(&host, &password, bind.as_deref());
    *state.client.lock().map_err(|e| e.to_string())? = client;
    Ok(())
}

/// Read-only GoForm query (pre-auth). `cmd` may be a comma-separated key list.
#[tauri::command]
fn goform_get(state: State<AppState>, cmd: String, multi: bool) -> Result<Value, String> {
    state.client()?.get_cmd(&cmd, multi).map(to_value)
}

/// Authenticated GoForm mutation. The core handles login + AD token + cookie.
#[tauri::command]
fn goform_post(
    state: State<AppState>,
    goform_id: String,
    params: HashMap<String, Value>,
) -> Result<Value, String> {
    let sparams: HashMap<String, String> = params
        .into_iter()
        .map(|(k, v)| {
            let s = match v {
                Value::String(s) => s,
                other => other.to_string(),
            };
            (k, s)
        })
        .collect();
    state.client()?.post_cmd(&goform_id, sparams, true).map(to_value)
}

#[tauri::command]
fn ensure_login(state: State<AppState>) -> Result<bool, String> {
    state.client()?.ensure_logged_in().map(|_| true)
}

#[tauri::command]
fn rotate(state: State<AppState>) -> Result<String, String> {
    state.client()?.rotate_and_reconnect()
}

/// Geo-IP lookup done in Rust so the webview needn't reach an external host.
#[tauri::command]
fn geo_lookup() -> Result<Value, String> {
    let url = "http://ip-api.com/json/?fields=status,message,country,countryCode,region,regionName,city,zip,lat,lon,timezone,isp,org,as,query";
    let c = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(6))
        .build()
        .map_err(|e| e.to_string())?;
    c.get(url)
        .send()
        .map_err(|e| e.to_string())?
        .json::<Value>()
        .map_err(|e| e.to_string())
}

fn main() {
    let default_client = ZTEClient::new("http://192.168.8.1", "", None);

    tauri::Builder::default()
        .manage(AppState {
            client: Mutex::new(default_client),
        })
        .invoke_handler(tauri::generate_handler![
            configure,
            goform_get,
            goform_post,
            ensure_login,
            rotate,
            geo_lookup
        ])
        .setup(|app| {
            // System tray with quick actions.
            let rotate_i = MenuItem::with_id(app, "rotate", "Rotate IP now", true, None::<&str>)?;
            let show_i = MenuItem::with_id(app, "show", "Show window", true, None::<&str>)?;
            let quit_i = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&rotate_i, &show_i, &quit_i])?;

            let mut tray = TrayIconBuilder::new()
                .menu(&menu)
                .tooltip("ZTE Control")
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "quit" => app.exit(0),
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    "rotate" => {
                        let state = app.state::<AppState>();
                        if let Ok(client) = state.client() {
                            std::thread::spawn(move || {
                                let _ = client.rotate_and_reconnect();
                            });
                        }
                    }
                    _ => {}
                });
            if let Some(icon) = app.default_window_icon().cloned() {
                tray = tray.icon(icon);
            }
            tray.build(app)?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
