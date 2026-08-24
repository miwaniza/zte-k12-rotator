//! zte-control core: modem WebUI client, band/bearer control, and multi-modem
//! fleet rotation. Shared by the CLI (`main.rs`) and the Tauri desktop app.

use base64::Engine as _;
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

static BAND_CYCLE_INDEX: AtomicUsize = AtomicUsize::new(0);

const ROTATION_MASKS: &[(&str, &str)] = &[
    ("Band 8 (900 MHz)",  "0x0000000000000080"),
    ("Band 3 (1800 MHz)", "0x0000000000000004"),
    ("Band 7 (2600 MHz)", "0x0000000000000040"),
    ("Band 20 (800 MHz)", "0x0000000000080000"),
    ("All Bands (Auto)",  "0xffffffffffffffff"),
];

/// "All bands" masks, passed with `is_*_band=1` to re-enable every 2G/3G ("gw")
/// and LTE band so the modem always has a usable RAT to camp on.
const GW_BAND_ALL: &str = "0xffffffffffffffff";
const LTE_BAND_ALL: &str = "0xffffffffffffffff";

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
    pub fn session_cookie(&self) -> Option<String> {
        self.session_cookie.lock().ok().and_then(|g| g.clone())
    }

    /// Capture the session cookie (first `name=value` pair) from a login response's
    /// `Set-Cookie` header(s). Firmware-agnostic: works for the MF920U's `stok` and
    /// any other ZTE session cookie name.
    pub fn capture_session_cookie(&self, resp: &reqwest::blocking::Response) {
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

    /// Apply an LTE band selection while ALWAYS keeping 2G/3G ("gw") bands
    /// enabled as a fallback. Sending `is_gw_band=0, gw_band_mask=0` (the old
    /// behavior) could leave the modem with no usable RAT and strand it in
    /// NO_SERVICE when the chosen LTE band has no coverage.
    fn select_bands(&self, lte_mask: &str) -> Result<HashMap<String, serde_json::Value>, String> {
        let mut params = HashMap::new();
        params.insert("is_gw_band".to_string(), "1".to_string());
        params.insert("gw_band_mask".to_string(), GW_BAND_ALL.to_string());
        params.insert("is_lte_band".to_string(), "1".to_string());
        params.insert("lte_band_mask".to_string(), lte_mask.to_string());
        self.post_cmd("BAND_SELECT", params, true)
    }

    pub fn lock_bands(&self, bands: &[String]) -> Result<String, String> {
        let mut mask: u64 = 0;
        let mut all = false;
        for b in bands {
            let s = b.to_uppercase();
            if s == "B3" || s == "3" { mask |= 0x4; }
            else if s == "B7" || s == "7" { mask |= 0x40; }
            else if s == "B8" || s == "8" { mask |= 0x80; }
            else if s == "B20" || s == "20" { mask |= 0x80000; }
            else if s == "ALL" { all = true; }
        }
        // Empty/unrecognized selection is treated as ALL rather than "disable
        // every LTE band", which would strand the modem.
        let hex_mask = if all || mask == 0 {
            LTE_BAND_ALL.to_string()
        } else {
            format!("0x{:016x}", mask)
        };
        let res = self.select_bands(&hex_mask)?;
        Ok(serde_json::to_string(&res).unwrap_or_default())
    }

    /// Recovery: re-enable ALL bands (2G/3G + every LTE band) and clear any cell
    /// lock, returning the modem to unrestricted auto selection. Undoes a narrow
    /// band lock that left it in NO_SERVICE.
    pub fn unlock_bands(&self) -> Result<String, String> {
        let _ = self.unlock_cell();
        let res = self.select_bands(LTE_BAND_ALL)?;
        Ok(serde_json::to_string(&res).unwrap_or_default())
    }

    /// Poll until the modem registers on some network (`network_type` present and
    /// not NO_SERVICE/LIMITED_SERVICE). Dialing before this is a no-op, so the
    /// heal path waits for it before issuing CONNECT_NETWORK.
    fn wait_registered(&self, tries: u32) -> bool {
        for _ in 0..tries {
            if let Ok(st) = self.get_cmd("network_type", false) {
                let nt = st.get("network_type").and_then(|v| v.as_str()).unwrap_or("");
                if !nt.is_empty() && nt != "NO_SERVICE" && !nt.starts_with("LIMITED_SERVICE") {
                    return true;
                }
            }
            thread::sleep(Duration::from_secs(1));
        }
        false
    }

    /// Poll for a live bearer. Success is gated on `ppp_status == ppp_connected`,
    /// which is readable without a web session. `wan_ipaddr` is auth-gated AND the
    /// web session can drop during a bearer reset, so we do NOT gate on it (that
    /// caused false "no bearer" failures); we re-auth best-effort only to report
    /// the new IP.
    fn wait_bearer(&self, tries: u32) -> Option<String> {
        for _ in 0..tries {
            thread::sleep(Duration::from_millis(1000));
            let ppp = self
                .get_cmd("ppp_status", false)
                .ok()
                .and_then(|m| m.get("ppp_status").and_then(|v| v.as_str()).map(|s| s.to_string()))
                .unwrap_or_default();
            if ppp == "ppp_connected" {
                let _ = self.ensure_logged_in();
                let ip = self
                    .get_cmd("wan_ipaddr", false)
                    .ok()
                    .and_then(|m| m.get("wan_ipaddr").and_then(|v| v.as_str()).map(|s| s.to_string()))
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "connected".to_string());
                return Some(ip);
            }
        }
        None
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

        // 2. Select target LTE band (keeps 2G/3G as fallback via select_bands)
        let _ = self.select_bands(band_mask);

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

        // 6. Wait for the bearer on the new band, or a 2G/3G fallback. Re-registration
        //    after a forced band change takes ~10-20s (measured), so give it room.
        if let Some(ip) = self.wait_bearer(20) {
            return Ok(ip);
        }

        // 7. Auto-heal: the target band may have no coverage here. Restore ALL bands
        //    so the modem can re-register on anything (incl. 3G), and give a full
        //    re-scan + register + auto-dial time. Guarantees a rotation can never
        //    strand the modem in NO_SERVICE (which would take it out of the fleet).
        println!("[!] bearer did not return on {}; restoring all bands", band_name);
        let _ = self.unlock_bands();
        // Register FIRST (on any RAT, incl. 3G), THEN dial -- issuing CONNECT while
        // still NO_SERVICE is a no-op that wastes the window.
        self.wait_registered(30);
        let mut p3 = HashMap::new();
        p3.insert("notCallback".to_string(), "true".to_string());
        let _ = self.post_cmd("CONNECT_NETWORK", p3, true);
        match self.wait_bearer(25) {
            Some(ip) => Ok(ip),
            None => Err("rotation failed: no bearer even after restoring all bands".to_string()),
        }
    }
}

pub fn chrono_ms() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

pub fn decode_bands(mask_str: &str) -> String {
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

pub fn get_first_non_empty<'a>(map: &'a HashMap<String, serde_json::Value>, keys: &[&str], default_val: &'a str) -> &'a str {
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


// ---------------------------------------------------------------------------
// Multi-modem "make-before-break" rotation (docs/multi_modem_rotation.md)
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize, Debug, Clone)]
pub struct FleetConfig {
    /// How long an ACTIVE modem serves before we rotate its peer.
    #[serde(default = "default_dwell")]
    pub dwell_seconds: u64,
    /// How long to wait for a just-rotated modem to become solid before giving up.
    #[serde(default = "default_solid_timeout")]
    pub solid_timeout_seconds: u64,
    /// Optional URL fetched source-bound through a modem to prove real internet.
    /// Should return quickly (a 204/generate_204 endpoint is ideal).
    #[serde(default)]
    pub probe_url: Option<String>,
    pub modems: Vec<ModemConfig>,
}

#[derive(serde::Deserialize, Debug, Clone)]
pub struct ModemConfig {
    pub name: String,
    pub host: String,
    pub password: String,
    /// Host address on this modem's subnet; control + probe traffic is bound to it.
    pub bind_ip: String,
    /// Windows: the modem interface's InterfaceIndex (Get-NetAdapter).
    #[serde(default)]
    pub iface_index: Option<u32>,
    /// Linux: the modem network interface name (e.g. "usb0").
    #[serde(default)]
    pub iface_name: Option<String>,
    /// Linux/macOS: this modem's gateway IP for its default route.
    #[serde(default)]
    pub gateway: Option<String>,
}

fn default_dwell() -> u64 { 90 }
fn default_solid_timeout() -> u64 { 60 }

const METRIC_ACTIVE: u32 = 10;
const METRIC_STANDBY: u32 = 9000;

struct FleetModem {
    cfg: ModemConfig,
    client: ZTEClient,
}

impl FleetModem {
    /// A modem is "solid" only if its bearer is up with a routable IP and, when a
    /// probe URL is configured, an actual request succeeds *through this modem*.
    fn is_solid(&self, probe: Option<&str>) -> bool {
        // ppp_status is readable without a web session; wan_ipaddr is auth-gated,
        // so we don't rely on it here. The source-bound probe is the definitive
        // "real internet" check.
        let ppp = self
            .client
            .get_cmd("ppp_status", false)
            .ok()
            .and_then(|m| m.get("ppp_status").and_then(|v| v.as_str()).map(|s| s.to_string()))
            .unwrap_or_default();
        if ppp != "ppp_connected" {
            return false;
        }
        match probe {
            Some(url) => probe_through(&self.cfg.bind_ip, url),
            None => true,
        }
    }
}

/// Issue an HTTP GET bound to `bind_ip` so it egresses through that modem's
/// interface, proving end-to-end internet rather than just a local bearer.
fn probe_through(bind_ip: &str, url: &str) -> bool {
    let ip: IpAddr = match bind_ip.parse() {
        Ok(i) => i,
        Err(_) => return false,
    };
    let client = match reqwest::blocking::Client::builder()
        .local_address(ip)
        .timeout(Duration::from_secs(4))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    match client.get(url).send() {
        Ok(r) => r.status().is_success() || r.status().is_redirection(),
        Err(_) => false,
    }
}

fn run_cmd(prog: &str, args: &[&str]) -> Result<(), String> {
    let out = std::process::Command::new(prog)
        .args(args)
        .output()
        .map_err(|e| format!("`{}` failed to start: {}", prog, e))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "`{}` exited {}: {}",
            prog,
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Set one modem's default-route preference (lower metric = preferred).
fn set_metric(m: &ModemConfig, metric: u32) -> Result<(), String> {
    if cfg!(target_os = "windows") {
        let idx = m
            .iface_index
            .ok_or_else(|| format!("modem '{}' needs iface_index on Windows", m.name))?;
        let arg = format!(
            "Set-NetIPInterface -InterfaceIndex {} -InterfaceMetric {}",
            idx, metric
        );
        run_cmd("powershell", &["-NoProfile", "-Command", &arg])
    } else if cfg!(target_os = "linux") {
        let dev = m
            .iface_name
            .clone()
            .ok_or_else(|| format!("modem '{}' needs iface_name on Linux", m.name))?;
        let gw = m
            .gateway
            .clone()
            .ok_or_else(|| format!("modem '{}' needs gateway on Linux", m.name))?;
        run_cmd(
            "ip",
            &[
                "route", "replace", "default", "via", &gw, "dev", &dev, "metric",
                &metric.to_string(),
            ],
        )
    } else {
        // macOS has a single default route (no per-interface metric); only the
        // active modem's gateway is installed as THE default.
        if metric == METRIC_ACTIVE {
            let gw = m
                .gateway
                .clone()
                .ok_or_else(|| format!("modem '{}' needs gateway on macOS", m.name))?;
            run_cmd("route", &["-n", "change", "default", &gw])
        } else {
            Ok(())
        }
    }
}

/// Make `active` the preferred uplink and demote all the others.
fn apply_active(modems: &[FleetModem], active: usize) -> Result<(), String> {
    set_metric(&modems[active].cfg, METRIC_ACTIVE)?;
    for (i, m) in modems.iter().enumerate() {
        if i != active {
            set_metric(&m.cfg, METRIC_STANDBY)?;
        }
    }
    Ok(())
}

fn wait_until_solid(m: &FleetModem, probe: Option<&str>, timeout_secs: u64) -> bool {
    let mut waited = 0u64;
    while waited < timeout_secs {
        if m.is_solid(probe) {
            return true;
        }
        thread::sleep(Duration::from_secs(2));
        waited += 2;
    }
    false
}

fn first_solid_other(modems: &[FleetModem], not: usize, probe: Option<&str>) -> Option<usize> {
    modems
        .iter()
        .enumerate()
        .find(|(i, m)| *i != not && m.is_solid(probe))
        .map(|(i, _)| i)
}

pub fn fleet_rotate(cfg: FleetConfig, once: bool) -> Result<(), String> {
    if cfg.modems.len() < 2 {
        return Err("fleet-rotate needs at least 2 modems in the config".to_string());
    }
    let probe = cfg.probe_url.as_deref();
    let modems: Vec<FleetModem> = cfg
        .modems
        .iter()
        .map(|mc| FleetModem {
            cfg: mc.clone(),
            client: ZTEClient::new(&mc.host, &mc.password, Some(&mc.bind_ip)),
        })
        .collect();

    // Pick an initial ACTIVE that is already solid.
    let mut active = (0..modems.len())
        .find(|&i| modems[i].is_solid(probe))
        .ok_or("no modem has a solid connection to start from")?;
    apply_active(&modems, active)?;
    println!("[fleet] ACTIVE = {}", modems[active].cfg.name);

    loop {
        // Rotate the next non-active modem (round-robin for N > 2).
        let standby = (active + 1) % modems.len();
        println!("[fleet] rotating STANDBY = {}", modems[standby].cfg.name);
        match modems[standby].rotate() {
            Ok(ip) => println!("[fleet]   {} -> IP {}", modems[standby].cfg.name, ip),
            Err(e) => println!("[fleet]   rotate error on {}: {}", modems[standby].cfg.name, e),
        }

        if wait_until_solid(&modems[standby], probe, cfg.solid_timeout_seconds) {
            // Make-before-break: the new path is up before we drop the old one.
            apply_active(&modems, standby)?;
            active = standby;
            println!("[fleet] SWAPPED -> ACTIVE = {}", modems[active].cfg.name);
        } else {
            println!(
                "[fleet] {} not solid after rotate; keeping ACTIVE = {}",
                modems[standby].cfg.name, modems[active].cfg.name
            );
        }

        if once {
            break;
        }

        // Dwell, watching the active modem; bail early if it drops.
        let mut waited = 0u64;
        while waited < cfg.dwell_seconds {
            thread::sleep(Duration::from_secs(3));
            waited += 3;
            if !modems[active].is_solid(probe) {
                break;
            }
        }

        // Emergency: active lost its bearer -> swap to any solid peer immediately.
        if !modems[active].is_solid(probe) {
            if let Some(peer) = first_solid_other(&modems, active, probe) {
                apply_active(&modems, peer)?;
                println!(
                    "[fleet] EMERGENCY swap -> ACTIVE = {} (previous active lost bearer)",
                    modems[peer].cfg.name
                );
                active = peer;
            } else {
                println!("[fleet] WARNING: active lost bearer and no solid peer to swap to");
            }
        }
    }
    Ok(())
}

impl FleetModem {
    fn rotate(&self) -> Result<String, String> {
        self.client.rotate_and_reconnect()
    }
}

