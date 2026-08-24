use base64::Engine as _;
use clap::{Parser, Subcommand};
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tiny_http::{Header, Method, Response, Server};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const EMBEDDED_UI_HTML: &str = include_str!("../web/index.html");
const EMBEDDED_MANIFEST: &str = include_str!("../web/manifest.json");
const EMBEDDED_SW_JS: &str = include_str!("../web/sw.js");
const EMBEDDED_ICON_SVG: &str = include_str!("../web/icon.svg");

static BAND_CYCLE_INDEX: AtomicUsize = AtomicUsize::new(0);

const ROTATION_MASKS: &[(&str, &str)] = &[
    ("Band 8 (900 MHz)",  "0x0000000000000080"),
    ("Band 3 (1800 MHz)", "0x0000000000000004"),
    ("Band 7 (2600 MHz)", "0x0000000000000040"),
    ("Band 20 (800 MHz)", "0x0000000000080000"),
    ("All Bands (Auto)",  "0x00000000000800c4"),
];

#[derive(Parser, Debug)]
#[command(name = "zte-control", author, version = VERSION, about = "Universal Controller, IP & Region Rotator for ZTE K12 (ZX297520)")]
pub struct Cli {
    #[arg(long, default_value = "http://192.168.8.1", help = "Router base URL")]
    pub host: String,

    #[arg(short, long, default_value = "353FALM5", help = "WebUI admin password")]
    pub password: String,

    #[arg(long, help = "Optional source IP to bind to")]
    pub bind_ip: Option<String>,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Show current cellular radio signal metrics & device status
    Status,

    /// Live monitor signal levels in terminal
    Monitor {
        #[arg(short, long, default_value_t = 2.0, help = "Polling interval in seconds")]
        interval: f64,
    },

    /// Lock to specific LTE band(s) (e.g. B3, B7, B8, B20, or ALL)
    LockBand {
        #[arg(required = true, help = "Bands to lock (e.g. B3 B7 or ALL)")]
        bands: Vec<String>,
    },

    /// Lock to specific cell tower sector (EARFCN + PCI)
    LockCell {
        #[arg(short, long, help = "LTE Downlink EARFCN channel (e.g. 1650 for B3, 3000 for B7)")]
        earfcn: u32,

        #[arg(short, long, help = "Physical Cell ID (0-503)")]
        pci: u32,

        #[arg(short, long, help = "Automatically cycle RF connection after locking")]
        reconnect: bool,
    },

    /// Clear cell lock (return to auto cell selection)
    UnlockCell {
        #[arg(short, long, help = "Automatically cycle RF connection")]
        reconnect: bool,
    },

    /// Force full band-hop + RF disconnect & reconnect to rotate IP & Region
    Reconnect,

    /// Rotate to next LTE frequency band + cell and obtain a guaranteed new IP
    Rotate,

    /// Check for application updates from GitHub
    CheckUpdate,

    /// Manage background Windows / macOS service
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },

    /// Launch built-in Web Control Dashboard with automated CORS proxy & PWA
    Ui {
        #[arg(short, long, default_value_t = 8080, help = "Local HTTP server port")]
        port: u16,

        #[arg(long, help = "Do not automatically open browser")]
        no_open: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum ServiceAction {
    Install,
    Uninstall,
    Start,
    Stop,
}

#[derive(Debug, Clone)]
pub struct ZTEClient {
    pub base_url: String,
    pub password: String,
    pub client: Client,
    /// Captured session cookie (e.g. `stok=...`) replayed on every request.
    /// The MF920U sets a malformed cookie (`HttpOnly=1`, invalid `Expires`) that
    /// reqwest's cookie store silently drops, so we carry it manually instead.
    session_cookie: Arc<Mutex<Option<String>>>,
}

impl ZTEClient {
    pub fn new(host: &str, password: &str, bind_ip: Option<&str>) -> Self {
        let base_url = host.trim_end_matches('/').to_string();
        // Session is carried manually via `session_cookie` (see the field docs),
        // so the built-in cookie store is disabled to avoid it clobbering our header.
        let mut builder = Client::builder()
            .cookie_store(false)
            .timeout(Duration::from_secs(6));

        if let Some(ip_str) = bind_ip {
            if let Ok(ip) = ip_str.parse::<IpAddr>() {
                builder = builder.local_address(ip);
            }
        }

        let client = builder.build().unwrap_or_else(|_| Client::new());

        Self {
            base_url,
            password: password.to_string(),
            client,
            session_cookie: Arc::new(Mutex::new(None)),
        }
    }

    /// The current session cookie pair (`name=value`), if logged in.
    fn session_cookie(&self) -> Option<String> {
        self.session_cookie.lock().ok().and_then(|g| g.clone())
    }

    /// Capture the session cookie (first `name=value` pair) from a login response's
    /// `Set-Cookie` header(s). Firmware-agnostic: works for the MF920U's `stok` and
    /// any other ZTE session cookie name.
    fn capture_session_cookie(&self, resp: &reqwest::blocking::Response) {
        for hv in resp.headers().get_all(reqwest::header::SET_COOKIE).iter() {
            if let Ok(s) = hv.to_str() {
                if let Some(pair) = s.split(';').next().map(|p| p.trim().to_string()) {
                    if pair.contains('=') {
                        if let Ok(mut g) = self.session_cookie.lock() {
                            *g = Some(pair);
                        }
                        return;
                    }
                }
            }
        }
    }

    pub fn sha256_hex_upper(input: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(input.as_bytes());
        let result = hasher.finalize();
        hex::encode(result).to_uppercase()
    }

    /// Lowercase hex MD5 -- the `hex_md5()` primitive the MF920U-class WebUI uses.
    pub fn md5_hex_lower(input: &str) -> String {
        format!("{:x}", md5::compute(input.as_bytes()))
    }

    /// Read the device firmware build string (`wa_inner_version`); this is pre-auth
    /// on both firmware families, so it can drive the auth-scheme selection below.
    fn firmware_version(&self) -> String {
        self.get_cmd("wa_inner_version", false)
            .ok()
            .and_then(|m| {
                m.get("wa_inner_version")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_default()
    }

    /// True for the K12/ZX297520 firmware, which authenticates with the SHA-256
    /// login + SHA-256 AD scheme. Everything else (e.g. the MF920U UFI family,
    /// `BD_MACTEXPKMF920U...`, whose WebUI sets `PASSWORD_ENCODE=true`) uses the
    /// Base64 login + MD5 AD scheme.
    fn is_k12_firmware(&self) -> bool {
        self.firmware_version().contains("K12")
    }

    pub fn get_cmd(&self, cmd: &str, multi: bool) -> Result<HashMap<String, serde_json::Value>, String> {
        let multi_flag = if multi { "&multi_data=1" } else { "" };
        let url = format!(
            "{}/goform/goform_get_cmd_process?cmd={}{}&isTest=false&_={}",
            self.base_url,
            cmd,
            multi_flag,
            chrono_ms()
        );

        let mut req = self
            .client
            .get(&url)
            .header("X-Requested-With", "XMLHttpRequest")
            .header("Referer", format!("{}/index.html", self.base_url));
        if let Some(cookie) = self.session_cookie() {
            req = req.header("Cookie", cookie);
        }
        let resp = req.send().map_err(|e| format!("HTTP GET error: {}", e))?;

        let json_map: HashMap<String, serde_json::Value> = resp
            .json()
            .map_err(|e| format!("JSON decode error: {}", e))?;
        Ok(json_map)
    }

    /// Anti-CSRF token required by sensitive SET commands. Both firmware families
    /// hash the live `wa_inner_version` against a fresh per-request `RD`, but differ
    /// in algorithm:
    ///   * K12/ZX297520: `AD = sha256_upper( sha256_upper(wa_inner_version) + RD )`
    ///   * MF920U-class: `AD = md5_lower( md5_lower(wa_inner_version + cr_version) + RD )`
    /// The version is read live from the device rather than hardcoded, so a firmware
    /// revision bump does not silently break the token.
    pub fn get_ad_token(&self) -> Result<String, String> {
        let ver = self.get_cmd("wa_inner_version,cr_version", true)?;
        let wa = ver
            .get("wa_inner_version")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let cr = ver
            .get("cr_version")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let rd = self
            .get_cmd("RD", false)?
            .get("RD")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let ad = if wa.contains("K12") {
            let fw_hash = Self::sha256_hex_upper(&wa);
            Self::sha256_hex_upper(&format!("{}{}", fw_hash, rd))
        } else {
            let ver_hash = Self::md5_hex_lower(&format!("{}{}", wa, cr));
            Self::md5_hex_lower(&format!("{}{}", ver_hash, rd))
        };
        Ok(ad)
    }

    pub fn login(&self) -> Result<bool, String> {
        let url = format!("{}/goform/goform_set_cmd_process", self.base_url);
        let mut params = HashMap::new();
        params.insert("isTest".to_string(), "false".to_string());
        params.insert("goformId".to_string(), "LOGIN".to_string());

        if self.is_k12_firmware() {
            // K12/ZX297520: password = sha256_upper( sha256_upper(pw) + LD ),
            // using the LD challenge, and the session opts into save_login.
            let ld = self
                .get_cmd("LD", false)?
                .get("LD")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let p1 = Self::sha256_hex_upper(&self.password);
            let password_hash = Self::sha256_hex_upper(&format!("{}{}", p1, ld));
            params.insert("password".to_string(), password_hash);
            params.insert("save_login".to_string(), "1".to_string());
        } else {
            // MF920U-class (WebUI config PASSWORD_ENCODE=true): password is just
            // Base64 of the plaintext -- no LD challenge, no SHA hashing.
            let password_enc =
                base64::engine::general_purpose::STANDARD.encode(self.password.as_bytes());
            params.insert("password".to_string(), password_enc);
        }

        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/x-www-form-urlencoded; charset=UTF-8")
            .header("X-Requested-With", "XMLHttpRequest")
            .header("Referer", format!("{}/index.html", self.base_url))
            .form(&params)
            .send()
            .map_err(|e| format!("Login POST error: {}", e))?;

        // Capture the session cookie before the body consumes the response.
        self.capture_session_cookie(&resp);

        let res_map: HashMap<String, serde_json::Value> = resp
            .json()
            .map_err(|e| format!("JSON decode error: {}", e))?;

        if let Some(r) = res_map.get("result").and_then(|v| v.as_str()) {
            if r == "0" || r == "4" {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn ensure_logged_in(&self) -> Result<(), String> {
        // A session is only usable if we actually hold its cookie. The modem reports
        // loginfo="ok" for any caller from an already-logged-in client IP -- even one
        // with no cookie -- but SET commands still require the cookie. So trust
        // loginfo only when we already hold a session cookie; otherwise (re)login to
        // capture one. Without this, a second CLI invocation reuses the lingering
        // IP session, skips login, and every SET fails for lack of the cookie.
        if self.session_cookie().is_some() {
            let status = self.get_cmd("loginfo", false)?;
            if status.get("loginfo").and_then(|v| v.as_str()) == Some("ok") {
                return Ok(());
            }
        }
        if !self.login()? {
            return Err("Failed to authenticate to ZTE WebUI".to_string());
        }
        Ok(())
    }

    pub fn post_cmd(&self, goform_id: &str, mut params: HashMap<String, String>, with_ad: bool) -> Result<HashMap<String, serde_json::Value>, String> {
        self.ensure_logged_in()?;

        if with_ad {
            let ad_token = self.get_ad_token()?;
            params.insert("AD".to_string(), ad_token);
        }

        params.insert("isTest".to_string(), "false".to_string());
        params.insert("goformId".to_string(), goform_id.to_string());

        let url = format!("{}/goform/goform_set_cmd_process", self.base_url);
        let mut req = self
            .client
            .post(&url)
            .header("Content-Type", "application/x-www-form-urlencoded; charset=UTF-8")
            .header("X-Requested-With", "XMLHttpRequest")
            .header("Referer", format!("{}/index.html", self.base_url))
            .form(&params);
        if let Some(cookie) = self.session_cookie() {
            req = req.header("Cookie", cookie);
        }
        let resp = req.send().map_err(|e| format!("POST command error: {}", e))?;

        let res_map: HashMap<String, serde_json::Value> = resp
            .json()
            .map_err(|e| format!("JSON response error: {}", e))?;
        Ok(res_map)
    }

    pub fn get_status(&self) -> Result<HashMap<String, serde_json::Value>, String> {
        self.ensure_logged_in()?;
        let keys = "wa_inner_version,hardware_version,imei,network_provider,network_type,network_lte_rsrp,lte_rsrp,lte_rsrq,lte_snr,network_sinr,lte_rssi,lte_band_lock,wan_active_band,wan_active_channel,lte_pci,cell_id,network_cell_id,wan_ipaddr,ppp_status,strFullName,strShortName";
        self.get_cmd(keys, true)
    }

    pub fn lock_bands(&self, bands: &[String]) -> Result<String, String> {
        let mut mask: u64 = 0;
        for b in bands {
            let s = b.to_uppercase();
            if s == "B3" || s == "3" { mask |= 0x4; }
            else if s == "B7" || s == "7" { mask |= 0x40; }
            else if s == "B8" || s == "8" { mask |= 0x80; }
            else if s == "B20" || s == "20" { mask |= 0x80000; }
            else if s == "ALL" { mask |= 0x800c4; }
        }

        let hex_mask = format!("0x{:016x}", mask);
        let mut params = HashMap::new();
        params.insert("is_gw_band".to_string(), "0".to_string());
        params.insert("gw_band_mask".to_string(), "0".to_string());
        params.insert("is_lte_band".to_string(), "1".to_string());
        params.insert("lte_band_mask".to_string(), hex_mask);

        let res = self.post_cmd("BAND_SELECT", params, true)?;
        Ok(serde_json::to_string(&res).unwrap_or_default())
    }

    pub fn lock_cell(&self, earfcn: u32, pci: u32) -> Result<String, String> {
        let mut params = HashMap::new();
        params.insert("lte_earfcn_lock".to_string(), earfcn.to_string());
        params.insert("lte_pci_lock".to_string(), pci.to_string());

        let res = self.post_cmd("LTE_LOCK_CELL_SET", params, true)?;
        Ok(serde_json::to_string(&res).unwrap_or_default())
    }

    pub fn unlock_cell(&self) -> Result<String, String> {
        let mut params = HashMap::new();
        params.insert("lte_earfcn_lock".to_string(), "0".to_string());
        params.insert("lte_pci_lock".to_string(), "0".to_string());
        let res = self.post_cmd("LTE_LOCK_CELL_SET", params, true)?;
        Ok(serde_json::to_string(&res).unwrap_or_default())
    }

    /// Combined Band-Hop + Carrier Bearer Reset (Guaranteed IP & Region change)
    pub fn rotate_and_reconnect(&self) -> Result<String, String> {
        let idx = BAND_CYCLE_INDEX.fetch_add(1, Ordering::SeqCst) % ROTATION_MASKS.len();
        let (band_name, band_mask) = ROTATION_MASKS[idx];
        println!("[*] Rotating to frequency {}: mask {}", band_name, band_mask);

        // 1. Clear cell lock
        let _ = self.unlock_cell();

        // 2. Select target frequency band to force gateway handover
        let mut p_band = HashMap::new();
        p_band.insert("is_gw_band".to_string(), "0".to_string());
        p_band.insert("gw_band_mask".to_string(), "0".to_string());
        p_band.insert("is_lte_band".to_string(), "1".to_string());
        p_band.insert("lte_band_mask".to_string(), band_mask.to_string());
        let _ = self.post_cmd("BAND_SELECT", p_band, true);

        // 3. Disconnect cellular session
        let mut p1 = HashMap::new();
        p1.insert("notCallback".to_string(), "true".to_string());
        let _ = self.post_cmd("DISCONNECT_NETWORK", p1, true);

        // 4. Guard sleep so PGW drops old IP lease
        thread::sleep(Duration::from_millis(1600));

        // 5. Connect cellular session
        let mut p2 = HashMap::new();
        p2.insert("notCallback".to_string(), "true".to_string());
        let _ = self.post_cmd("CONNECT_NETWORK", p2, true);

        // 6. Wait for PPP connected and return new IP
        for _ in 0..8 {
            thread::sleep(Duration::from_millis(1000));
            if let Ok(st) = self.get_cmd("wan_ipaddr,ppp_status", false) {
                let ppp = st.get("ppp_status").and_then(|v| v.as_str()).unwrap_or("");
                let ip = st.get("wan_ipaddr").and_then(|v| v.as_str()).unwrap_or("");
                if ppp == "ppp_connected" && !ip.is_empty() {
                    return Ok(ip.to_string());
                }
            }
        }
        Ok("reconnected".to_string())
    }
}

fn chrono_ms() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn decode_bands(mask_str: &str) -> String {
    let raw = mask_str.trim_start_matches("0x").trim_start_matches("0X");
    if let Ok(mask) = u64::from_str_radix(raw, 16) {
        let mut list = Vec::new();
        if mask & 0x4 != 0 { list.push("B3 (1800)"); }
        if mask & 0x40 != 0 { list.push("B7 (2600)"); }
        if mask & 0x80 != 0 { list.push("B8 (900)"); }
        if mask & 0x80000 != 0 { list.push("B20 (800)"); }
        if list.is_empty() { "None".to_string() } else { list.join(", ") }
    } else {
        "Auto".to_string()
    }
}

fn get_first_non_empty<'a>(map: &'a HashMap<String, serde_json::Value>, keys: &[&str], default_val: &'a str) -> &'a str {
    for k in keys {
        if let Some(v) = map.get(*k) {
            if let Some(s) = v.as_str() {
                if !s.trim().is_empty() && s != "None" {
                    return s;
                }
            }
        }
    }
    default_val
}

pub fn check_for_updates() -> Result<(String, String, bool), String> {
    let client = Client::builder()
        .timeout(Duration::from_secs(4))
        .user_agent("zte-control-updater")
        .build()
        .map_err(|e| e.to_string())?;

    let resp = client
        .get("https://api.github.com/repos/miwaniza/zte-k12-rotator/releases/latest")
        .send()
        .map_err(|e| format!("Failed to reach GitHub API: {}", e))?;

    let json: serde_json::Value = resp.json().map_err(|e| e.to_string())?;
    let latest_tag = json
        .get("tag_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim_start_matches('v')
        .to_string();

    let has_update = !latest_tag.is_empty() && latest_tag != VERSION;
    Ok((VERSION.to_string(), latest_tag, has_update))
}

pub fn run_ui_server(client: Arc<ZTEClient>, port: u16, no_open: bool) {
    let server_addr = format!("0.0.0.0:{}", port);
    let server = Server::http(&server_addr).expect("Failed to start HTTP server");

    println!("============================================================");
    println!("  🚀 ZTE K12 Master Web Controller & PWA v{} Started", VERSION);
    println!("  👉 Dashboard: http://127.0.0.1:{}", port);
    println!("  📡 Router:    {}", client.base_url);
    println!("============================================================");

    if !no_open {
        let url = format!("http://127.0.0.1:{}", port);
        let _ = open::that(url);
    }

    let http_client = Client::builder()
        .timeout(Duration::from_secs(5))
        .user_agent("zte-control-server")
        .build()
        .unwrap_or_else(|_| Client::new());

    for mut request in server.incoming_requests() {
        let url_path = request.url().to_string();

        if url_path.starts_with("/manifest.json") {
            let response = Response::from_string(EMBEDDED_MANIFEST)
                .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/manifest+json; charset=utf-8"[..]).unwrap())
                .with_header(Header::from_bytes(&b"Cache-Control"[..], &b"no-cache, no-store, must-revalidate"[..]).unwrap())
                .with_header(Header::from_bytes(&b"Access-Control-Allow-Origin"[..], &b"*"[..]).unwrap());
            let _ = request.respond(response);
        } else if url_path.starts_with("/sw.js") {
            let response = Response::from_string(EMBEDDED_SW_JS)
                .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/javascript; charset=utf-8"[..]).unwrap())
                .with_header(Header::from_bytes(&b"Service-Worker-Allowed"[..], &b"/"[..]).unwrap())
                .with_header(Header::from_bytes(&b"Cache-Control"[..], &b"no-cache, no-store, must-revalidate"[..]).unwrap())
                .with_header(Header::from_bytes(&b"Access-Control-Allow-Origin"[..], &b"*"[..]).unwrap());
            let _ = request.respond(response);
        } else if url_path.starts_with("/icon.svg") {
            let response = Response::from_string(EMBEDDED_ICON_SVG)
                .with_header(Header::from_bytes(&b"Content-Type"[..], &b"image/svg+xml; charset=utf-8"[..]).unwrap())
                .with_header(Header::from_bytes(&b"Access-Control-Allow-Origin"[..], &b"*"[..]).unwrap());
            let _ = request.respond(response);
        } else if url_path.starts_with("/api/reconnect") || url_path.starts_with("/api/rotate") {
            let new_ip = client.rotate_and_reconnect().unwrap_or_else(|_| "reconnected".to_string());
            let json_res = format!("{{\"status\":\"success\",\"action\":\"rotated\",\"wan_ip\":\"{}\"}}", new_ip);
            let response = Response::from_string(json_res)
                .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/json; charset=utf-8"[..]).unwrap())
                .with_header(Header::from_bytes(&b"Cache-Control"[..], &b"no-cache, no-store, must-revalidate"[..]).unwrap())
                .with_header(Header::from_bytes(&b"Access-Control-Allow-Origin"[..], &b"*"[..]).unwrap());
            let _ = request.respond(response);
        } else if url_path.starts_with("/api/update/check") {
            let json_body = match check_for_updates() {
                Ok((current, latest, has_update)) => {
                    format!("{{\"current\":\"{}\",\"latest\":\"{}\",\"has_update\":{}}}", current, latest, has_update)
                }
                Err(e) => format!("{{\"error\":\"{}\"}}", e),
            };
            let response = Response::from_string(json_body)
                .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/json; charset=utf-8"[..]).unwrap())
                .with_header(Header::from_bytes(&b"Cache-Control"[..], &b"no-cache, no-store, must-revalidate"[..]).unwrap())
                .with_header(Header::from_bytes(&b"Access-Control-Allow-Origin"[..], &b"*"[..]).unwrap());
            let _ = request.respond(response);
        } else if url_path.starts_with("/api/geo") {
            let geo_data = http_client
                .get("http://ip-api.com/json/?fields=status,message,country,countryCode,region,regionName,city,zip,lat,lon,timezone,isp,org,as,query")
                .send()
                .and_then(|r| r.text())
                .unwrap_or_else(|_| {
                    http_client
                        .get("https://ipwho.is/")
                        .send()
                        .and_then(|r| r.text())
                        .unwrap_or_else(|_| "{\"status\":\"fail\"}".to_string())
                });

            let response = Response::from_string(geo_data)
                .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/json; charset=utf-8"[..]).unwrap())
                .with_header(Header::from_bytes(&b"Cache-Control"[..], &b"no-cache, no-store, must-revalidate"[..]).unwrap())
                .with_header(Header::from_bytes(&b"Access-Control-Allow-Origin"[..], &b"*"[..]).unwrap());
            let _ = request.respond(response);
        } else if url_path.starts_with("/goform/") {
            let target_url = format!("{}{}", client.base_url, url_path);

            let result_body = if *request.method() == Method::Post {
                let mut body_bytes = Vec::new();
                let _ = request.as_reader().read_to_end(&mut body_bytes);
                let mut req = client
                    .client
                    .post(&target_url)
                    .header("Content-Type", "application/x-www-form-urlencoded; charset=UTF-8")
                    .header("X-Requested-With", "XMLHttpRequest")
                    .header("Referer", format!("{}/index.html", client.base_url))
                    .body(body_bytes);
                if let Some(cookie) = client.session_cookie() {
                    req = req.header("Cookie", cookie);
                }
                match req.send() {
                    // Capture the session cookie from a login forwarded through the
                    // proxy, so subsequent forwarded SETs carry it (the cookie store
                    // is disabled; see ZTEClient::session_cookie).
                    Ok(r) => {
                        client.capture_session_cookie(&r);
                        r.text().unwrap_or_else(|e| format!("{{\"error\":\"{}\"}}", e))
                    }
                    Err(e) => format!("{{\"error\":\"{}\"}}", e),
                }
            } else {
                let mut req = client
                    .client
                    .get(&target_url)
                    .header("X-Requested-With", "XMLHttpRequest")
                    .header("Referer", format!("{}/index.html", client.base_url));
                if let Some(cookie) = client.session_cookie() {
                    req = req.header("Cookie", cookie);
                }
                req.send()
                    .and_then(|r| r.text())
                    .unwrap_or_else(|e| format!("{{\"error\":\"{}\"}}", e))
            };

            let response = Response::from_string(result_body)
                .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/json; charset=utf-8"[..]).unwrap())
                .with_header(Header::from_bytes(&b"Cache-Control"[..], &b"no-cache, no-store, must-revalidate"[..]).unwrap())
                .with_header(Header::from_bytes(&b"Access-Control-Allow-Origin"[..], &b"*"[..]).unwrap());
            let _ = request.respond(response);
        } else {
            let response = Response::from_string(EMBEDDED_UI_HTML)
                .with_header(Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]).unwrap())
                .with_header(Header::from_bytes(&b"Cache-Control"[..], &b"no-cache, no-store, must-revalidate"[..]).unwrap())
                .with_header(Header::from_bytes(&b"Pragma"[..], &b"no-cache"[..]).unwrap());
            let _ = request.respond(response);
        }
    }
}

fn handle_service_command(action: ServiceAction) {
    #[cfg(windows)]
    {
        use std::process::Command;
        let exe_path = std::env::current_exe().unwrap_or_default();
        let exe_str = exe_path.to_string_lossy().to_string();

        match action {
            ServiceAction::Install => {
                println!("[*] Registering Windows Scheduled Background Task: ZTEK12RotatorService");
                let status = Command::new("schtasks")
                    .args(&["/Create", "/TN", "ZTEK12RotatorService", "/TR", &format!("\"{}\" ui --no-open", exe_str), "/SC", "ONLOGON", "/RL", "HIGHEST", "/F"])
                    .status();
                if status.map(|s| s.success()).unwrap_or(false) {
                    println!("[+] Successfully installed background service!");
                    let _ = Command::new("schtasks").args(&["/Run", "/TN", "ZTEK12RotatorService"]).status();
                    println!("[+] Service started at http://127.0.0.1:8080");
                } else {
                    eprintln!("[-] Failed to install service. Try running as Administrator.");
                }
            }
            ServiceAction::Uninstall => {
                println!("[*] Removing Windows Scheduled Background Task: ZTEK12RotatorService");
                let _ = Command::new("schtasks").args(&["/End", "/TN", "ZTEK12RotatorService"]).status();
                let _ = Command::new("schtasks").args(&["/Delete", "/TN", "ZTEK12RotatorService", "/F"]).status();
                println!("[+] Service uninstalled.");
            }
            ServiceAction::Start => {
                let _ = Command::new("schtasks").args(&["/Run", "/TN", "ZTEK12RotatorService"]).status();
                println!("[+] Background task start requested.");
            }
            ServiceAction::Stop => {
                let _ = Command::new("schtasks").args(&["/End", "/TN", "ZTEK12RotatorService"]).status();
                println!("[+] Background task stopped.");
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        use std::fs;
        let home = std::env::var("HOME").unwrap_or_else(|_| "/Users".to_string());
        let plist_dir = format!("{}/Library/LaunchAgents", home);
        let plist_path = format!("{}/com.zte.rotator.plist", plist_dir);
        let log_dir = format!("{}/.zte-k12-rotator", home);
        let exe_path = std::env::current_exe().unwrap_or_default();
        let exe_str = exe_path.to_string_lossy().to_string();

        match action {
            ServiceAction::Install => {
                println!("[*] Installing macOS background service (LaunchAgent)...");
                let _ = fs::create_dir_all(&plist_dir);
                let _ = fs::create_dir_all(&log_dir);

                let plist_content = format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.zte.rotator</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
        <string>ui</string>
        <string>--no-open</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{}/service.log</string>
    <key>StandardErrorPath</key>
    <string>{}/service.err</string>
</dict>
</plist>"#, exe_str, log_dir, log_dir);

                if let Ok(_) = fs::write(&plist_path, plist_content) {
                    let _ = Command::new("launchctl").args(&["unload", "-w", &plist_path]).status();
                    let status = Command::new("launchctl").args(&["load", "-w", &plist_path]).status();
                    if status.map(|s| s.success()).unwrap_or(false) {
                        println!("[+] Successfully registered LaunchAgent at: {}", plist_path);
                        println!("[+] Background service active at http://127.0.0.1:8080");
                    } else {
                        eprintln!("[-] Failed to load LaunchAgent via launchctl.");
                    }
                } else {
                    eprintln!("[-] Failed to write plist file: {}", plist_path);
                }
            }
            ServiceAction::Uninstall => {
                println!("[*] Unloading and removing macOS LaunchAgent...");
                let _ = Command::new("launchctl").args(&["unload", "-w", &plist_path]).status();
                let _ = fs::remove_file(&plist_path);
                println!("[+] Service uninstalled.");
            }
            ServiceAction::Start => {
                let _ = Command::new("launchctl").args(&["start", "com.zte.rotator"]).status();
                println!("[+] LaunchAgent started.");
            }
            ServiceAction::Stop => {
                let _ = Command::new("launchctl").args(&["stop", "com.zte.rotator"]).status();
                println!("[+] LaunchAgent stopped.");
            }
        }
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        println!("[*] Service management: on Linux use systemd user service or nohup.");
        match action {
            ServiceAction::Install => println!("[+] Use: nohup zte-control ui --no-open >/dev/null 2>&1 &"),
            _ => println!("[+] Action noted."),
        }
    }
}

fn main() {
    let cli = Cli::parse();
    let client = Arc::new(ZTEClient::new(&cli.host, &cli.password, cli.bind_ip.as_deref()));

    match cli.command {
        None | Some(Commands::Status) => match client.get_status() {
            Ok(status) => {
                println!("============================================================");
                println!("           📡 ZTE K12 CELLULAR ROUTER STATUS v{}", VERSION);
                println!("============================================================");
                let hw = get_first_non_empty(&status, &["hardware_version"], "K12HW1.0");
                let fw = get_first_non_empty(&status, &["wa_inner_version"], "N/A");
                let imei = get_first_non_empty(&status, &["imei"], "N/A");
                let provider = get_first_non_empty(&status, &["network_provider", "strFullName", "strShortName"], "Unknown");
                let net_type = get_first_non_empty(&status, &["network_type"], "LTE");
                let ppp = get_first_non_empty(&status, &["ppp_status"], "connected");
                let wan_ip = get_first_non_empty(&status, &["wan_ipaddr"], "--");
                
                let active_band = get_first_non_empty(&status, &["wan_active_band"], "N/A");
                let active_channel = get_first_non_empty(&status, &["wan_active_channel", "lte_earfcn"], "N/A");
                let pci = get_first_non_empty(&status, &["lte_pci"], "--");
                let cell_id = get_first_non_empty(&status, &["cell_id", "network_cell_id"], "--");

                let rsrp = get_first_non_empty(&status, &["network_lte_rsrp", "lte_rsrp"], "--");
                let rssi = get_first_non_empty(&status, &["lte_rssi"], "--");
                let sinr = get_first_non_empty(&status, &["network_sinr", "lte_snr"], "--");
                let rsrq = get_first_non_empty(&status, &["lte_rsrq"], "--");
                
                let band_mask = get_first_non_empty(&status, &["lte_band_lock"], "0x0000800c4");
                let bands = decode_bands(band_mask);

                println!(" Device / FW:   {} | {}", hw, fw);
                println!(" IMEI:          {}", imei);
                println!(" Operator:      {} ({}, PPP: {}, IP: {})", provider, net_type, ppp, wan_ip);
                println!(" Active Tower:  Band: {} | EARFCN: {} | PCI: {} | Cell ID: {}", active_band, active_channel, pci, cell_id);
                println!(" Signal Levels: RSRP: {} dBm | RSSI: {} dBm", rsrp, rssi);
                println!(" Signal Quality:SINR: {} dB | RSRQ: {} dB", sinr, rsrq);
                println!(" Allowed Bands: {} (Mask: {})", bands, band_mask);
                println!("============================================================");
            }
            Err(e) => eprintln!("[-] Failed to fetch status: {}", e),
        },

        Some(Commands::Monitor { interval }) => {
            println!("[*] Starting live cellular monitor (every {:.1}s). Press Ctrl+C to stop.", interval);
            println!("{:<8} | {:<12} | {:<14} | {:<8} | {:<10} | {:<10} | {:<8} | {:<6}", "Time", "Operator", "Active Band", "EARFCN", "RSRP", "RSSI", "SINR", "PCI");
            println!("{}", "-".repeat(92));
            loop {
                if let Ok(st) = client.get_status() {
                    let ts = chrono_ms() / 1000 % 86400;
                    let hrs = ts / 3600;
                    let mins = (ts % 3600) / 60;
                    let secs = ts % 60;
                    let time_str = format!("{:02}:{:02}:{:02}", hrs, mins, secs);

                    let op = get_first_non_empty(&st, &["network_provider", "strShortName"], "N/A");
                    let band = get_first_non_empty(&st, &["wan_active_band"], "N/A");
                    let earfcn = get_first_non_empty(&st, &["wan_active_channel", "lte_earfcn"], "--");
                    let rsrp = get_first_non_empty(&st, &["network_lte_rsrp", "lte_rsrp"], "--");
                    let rssi = get_first_non_empty(&st, &["lte_rssi"], "--");
                    let sinr = get_first_non_empty(&st, &["network_sinr", "lte_snr"], "--");
                    let pci = get_first_non_empty(&st, &["lte_pci"], "--");

                    println!("{:<8} | {:<12} | {:<14} | {:<8} | {:<10} | {:<10} | {:<8} | {:<6}", time_str, op, band, earfcn, format!("{} dBm", rsrp), format!("{} dBm", rssi), format!("{} dB", sinr), pci);
                }
                thread::sleep(Duration::from_secs_f64(interval));
            }
        }

        Some(Commands::LockBand { bands }) => match client.lock_bands(&bands) {
            Ok(res) => println!("[+] Band lock result: {}", res),
            Err(e) => eprintln!("[-] Error locking band: {}", e),
        },

        Some(Commands::LockCell { earfcn, pci, reconnect }) => match client.lock_cell(earfcn, pci) {
            Ok(res) => {
                println!("[+] Cell lock result: {}", res);
                if reconnect {
                    let _ = client.rotate_and_reconnect();
                }
            }
            Err(e) => eprintln!("[-] Error locking cell: {}", e),
        },

        Some(Commands::UnlockCell { reconnect }) => match client.unlock_cell() {
            Ok(res) => {
                println!("[+] Unlock result: {}", res);
                if reconnect {
                    let _ = client.rotate_and_reconnect();
                }
            }
            Err(e) => eprintln!("[-] Error unlocking cell: {}", e),
        },

        Some(Commands::Reconnect) | Some(Commands::Rotate) => {
            match client.rotate_and_reconnect() {
                Ok(new_ip) => println!("[+] Cellular session rotated! New WAN IP: {}", new_ip),
                Err(e) => eprintln!("[-] Error during rotation: {}", e),
            }
        }

        Some(Commands::CheckUpdate) => match check_for_updates() {
            Ok((cur, latest, has_update)) => {
                println!("Current version: v{}", cur);
                println!("Latest release:  v{}", latest);
                if has_update {
                    println!("[+] 🚀 Update available! Run `irm https://raw.githubusercontent.com/miwaniza/zte-k12-rotator/main/install.ps1 | iex` to update.");
                } else {
                    println!("[+] You are on the latest version.");
                }
            }
            Err(e) => eprintln!("[-] Error checking updates: {}", e),
        },

        Some(Commands::Service { action }) => {
            handle_service_command(action);
        }

        Some(Commands::Ui { port, no_open }) => {
            run_ui_server(client, port, no_open);
        }
    }
}
