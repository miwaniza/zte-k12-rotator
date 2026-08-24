//! Tiny persistence for the connection settings so the app remembers the modem
//! and auto-connects on launch. Stored as JSON under %APPDATA%\zte-egui (or
//! $XDG_CONFIG_HOME / ~/.config elsewhere). Note: the password is stored in
//! plaintext, same posture as the web dashboard's localStorage.

use std::path::PathBuf;

#[derive(Clone)]
pub struct ConnSettings {
    pub host: String,
    pub password: String,
    pub bind_ip: String,
}

impl Default for ConnSettings {
    fn default() -> Self {
        Self {
            host: "http://192.168.8.1".to_string(),
            password: String::new(),
            bind_ip: String::new(),
        }
    }
}

fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME"))
        .or_else(|| std::env::var_os("HOME").map(|h| {
            let mut p = PathBuf::from(h);
            p.push(".config");
            p.into_os_string()
        }))?;
    let mut p = PathBuf::from(base);
    p.push("zte-egui");
    let _ = std::fs::create_dir_all(&p);
    p.push("conn.json");
    Some(p)
}

pub fn load() -> ConnSettings {
    let def = ConnSettings::default();
    let p = match config_path() {
        Some(p) => p,
        None => return def,
    };
    let txt = match std::fs::read_to_string(&p) {
        Ok(t) => t,
        Err(_) => return def,
    };
    let v: serde_json::Value = match serde_json::from_str(&txt) {
        Ok(v) => v,
        Err(_) => return def,
    };
    let s = |k: &str, d: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or(d).to_string();
    ConnSettings {
        host: s("host", &def.host),
        password: s("password", ""),
        bind_ip: s("bind_ip", ""),
    }
}

pub fn save(c: &ConnSettings) {
    if let Some(p) = config_path() {
        let v = serde_json::json!({
            "host": c.host,
            "password": c.password,
            "bind_ip": c.bind_ip,
        });
        let _ = std::fs::write(p, serde_json::to_string_pretty(&v).unwrap_or_default());
    }
}
