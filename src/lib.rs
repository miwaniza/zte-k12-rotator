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

/// How many band-hops `rotate_and_reconnect` will try before giving up on getting
/// an address different from the one the modem held beforehand.
pub const DEFAULT_ROTATE_ATTEMPTS: u32 = 3;

/// Progress sink. Long operations (rotation, fleet cycles) report through this so
/// a GUI -- which has no stdout to print to -- can show what is happening.
pub type Logger = Arc<dyn Fn(&str) + Send + Sync>;

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

#[derive(Clone)]
pub struct ZTEClient {
    pub base_url: String,
    pub password: String,
    pub client: Client,
    /// Captured session cookie (e.g. `stok=...`) replayed on every request.
    /// The MF920U sets a malformed cookie (`HttpOnly=1`, invalid `Expires`) that
    /// reqwest's cookie store silently drops, so we carry it manually instead.
    session_cookie: Arc<Mutex<Option<String>>>,
    firmware_version_cache: Arc<Mutex<Option<String>>>,
    /// Per-client cursor into `ROTATION_MASKS`. This used to be a process-global
    /// static, which made two modems in a fleet interleave one shared cycle
    /// instead of each walking the band list independently.
    band_cycle: Arc<AtomicUsize>,
    /// Where progress messages go; `println!` when unset (the CLI default).
    logger: Arc<Mutex<Option<Logger>>>,
}

/// Hand-written so the password is never printed by a `{:?}`.
impl std::fmt::Debug for ZTEClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ZTEClient")
            .field("base_url", &self.base_url)
            .field("logged_in", &self.session_cookie().is_some())
            .finish_non_exhaustive()
    }
}

impl ZTEClient {
    pub fn new(host: &str, password: &str, bind_ip: Option<&str>) -> Self {
        let base_url = host.trim_end_matches('/').to_string();
        // Session is carried manually via `session_cookie` (see the field docs),
        // so the built-in cookie store is disabled to avoid it clobbering our header.
        let mut builder = Client::builder()
            .cookie_store(false)
            .timeout(Duration::from_secs(6));

        if let Some(ip) = bind_ip.and_then(|s| s.parse::<IpAddr>().ok()) {
            builder = builder.local_address(ip);
        }

        let client = builder.build().unwrap_or_else(|_| Client::new());

        Self {
            base_url,
            password: password.to_string(),
            client,
            session_cookie: Arc::new(Mutex::new(None)),
            firmware_version_cache: Arc::new(Mutex::new(None)),
            band_cycle: Arc::new(AtomicUsize::new(0)),
            logger: Arc::new(Mutex::new(None)),
        }
    }

    /// Route this client's progress messages somewhere other than stdout. Shared
    /// with every clone of the client, so it can be set after construction.
    pub fn set_logger(&self, sink: Logger) {
        if let Ok(mut g) = self.logger.lock() {
            *g = Some(sink);
        }
    }

    fn log(&self, msg: &str) {
        let sink = self.logger.lock().ok().and_then(|g| g.clone());
        match sink {
            Some(f) => f(msg),
            None => println!("{}", msg),
        }
    }

    /// The current session cookie pair (`name=value`), if logged in.
    pub fn session_cookie(&self) -> Option<String> {
        self.session_cookie.lock().ok().and_then(|g| g.as_ref().cloned())
    }

    /// Capture the session cookie (first `name=value` pair) from a login response's
    /// `Set-Cookie` header(s). Firmware-agnostic: works for the MF920U's `stok` and
    /// any other ZTE session cookie name.
    pub fn capture_session_cookie(&self, resp: &reqwest::blocking::Response) {
        for hv in resp.headers().get_all(reqwest::header::SET_COOKIE).iter() {
            if let Ok(s) = hv.to_str() {
                if let Some(pair) = s.split(';').next().map(|p| p.trim()) {
                    if pair.contains('=') {
                        if let Ok(mut g) = self.session_cookie.lock() {
                            *g = Some(pair.to_string());
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
        if let Ok(guard) = self.firmware_version_cache.lock() {
            if let Some(ref ver) = *guard {
                return ver.clone();
            }
        }
        let ver = self.get_cmd("wa_inner_version", false)
            .ok()
            .and_then(|m| {
                m.get("wa_inner_version")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_default();
        if !ver.is_empty() {
            if let Ok(mut guard) = self.firmware_version_cache.lock() {
                *guard = Some(ver.clone());
            }
        }
        ver
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

    /// Request authentication token (AD) required by sensitive SET commands. Both firmware families
    /// hash the live `wa_inner_version` against a fresh per-request `RD`, but differ
    /// in algorithm:
    ///   * K12/ZX297520: `AD = sha256_upper( sha256_upper(wa_inner_version) + RD )`
    ///   * MF920U-class: `AD = md5_lower( md5_lower(wa_inner_version + cr_version) + RD )`
    ///
    /// The version is read live from the device rather than hardcoded, so a firmware
    /// revision bump does not silently break the token.
    pub fn get_ad_token(&self) -> Result<String, String> {
        let ver = self.get_cmd("wa_inner_version,cr_version", true)?;
        let mut wa = ver
            .get("wa_inner_version")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if wa.is_empty() {
            wa = self.firmware_version();
        }
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

        let ad = if self.is_k12_firmware() || wa.contains("K12") {
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
        if self.password.is_empty() {
            return Err("No password configured for WebUI authentication".to_string());
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
        let _ = self.ensure_logged_in();
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

    /// The WAN address the modem currently holds, if a session can read it.
    /// `wan_ipaddr` is auth-gated, so this is `None` both when there is no bearer
    /// and when there is no usable web session -- callers must not treat `None`
    /// as "disconnected".
    pub fn read_wan_ip(&self) -> Option<String> {
        let _ = self.ensure_logged_in();
        self.get_cmd("wan_ipaddr", false)
            .ok()
            .and_then(|m| m.get("wan_ipaddr").and_then(|v| v.as_str()).map(|s| s.to_string()))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && s != "0.0.0.0")
    }

    /// Poll for a live bearer. Success is gated on `ppp_status == ppp_connected`,
    /// which is readable without a web session. `wan_ipaddr` is auth-gated AND the
    /// web session can drop during a bearer reset, so we do NOT gate on it (that
    /// caused false "no bearer" failures); we re-auth best-effort only to report
    /// the new IP.
    ///
    /// `None` = no bearer came back. `Some(None)` = the bearer is up but its
    /// address could not be read; the two are deliberately distinct so a caller
    /// never reports an unverifiable rotation as a successful one.
    fn wait_bearer(&self, tries: u32) -> Option<Option<String>> {
        for _ in 0..tries {
            thread::sleep(Duration::from_millis(1000));
            let ppp = self
                .get_cmd("ppp_status", false)
                .ok()
                .and_then(|m| m.get("ppp_status").and_then(|v| v.as_str()).map(|s| s.to_string()))
                .unwrap_or_default();
            if ppp == "ppp_connected" {
                return Some(self.read_wan_ip());
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

    /// Point the modem at ONE LTE band, for tower discovery. Goes through
    /// `select_bands`, so 2G/3G stay enabled and an interrupted scan can never
    /// leave the modem with no RAT to camp on.
    pub fn scan_band(&self, lte_mask: &str) -> Result<(), String> {
        let _ = self.unlock_cell();
        self.select_bands(lte_mask).map(|_| ())
    }

    /// One band-hop + carrier bearer reset.
    ///
    /// `Ok(Some(ip))` -- bearer returned and its address was read.
    /// `Ok(None)`     -- bearer returned but the address is unreadable.
    /// `Err(_)`       -- no bearer came back, even after restoring all bands.
    fn rotate_once(&self) -> Result<Option<String>, String> {
        let idx = self.band_cycle.fetch_add(1, Ordering::SeqCst) % ROTATION_MASKS.len();
        let (band_name, band_mask) = ROTATION_MASKS[idx];
        self.log(&format!("[*] Rotating to frequency {}: mask {}", band_name, band_mask));

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
        self.log(&format!("[!] bearer did not return on {}; restoring all bands", band_name));
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

    /// Band-hop + bearer reset until the carrier hands out an address that differs
    /// from the one held before the rotation, up to `DEFAULT_ROTATE_ATTEMPTS`
    /// hops. Each retry advances to the next band, so a carrier that pins one
    /// address per band still gets away from the old one.
    pub fn rotate_and_reconnect(&self) -> Result<RotationOutcome, String> {
        self.rotate_verified(DEFAULT_ROTATE_ATTEMPTS)
    }

    /// `rotate_and_reconnect` with an explicit hop budget.
    pub fn rotate_verified(&self, max_attempts: u32) -> Result<RotationOutcome, String> {
        let budget = max_attempts.max(1);
        let previous = self.read_wan_ip();
        let mut repeated: Option<String> = None;
        let mut last_err: Option<String> = None;

        for attempt in 1..=budget {
            match self.rotate_once() {
                Ok(Some(ip)) => {
                    if previous.as_deref() != Some(ip.as_str()) {
                        return Ok(RotationOutcome::NewIp { ip, previous, attempts: attempt });
                    }
                    self.log(&format!(
                        "[!] attempt {}/{}: carrier re-issued the same address {}; hopping again",
                        attempt, budget, ip
                    ));
                    repeated = Some(ip);
                }
                // Without a readable address there is nothing to compare against,
                // so retrying would only churn the bearer blindly. Report honestly
                // that the rotation happened but could not be verified.
                Ok(None) => return Ok(RotationOutcome::BearerUpIpUnknown { attempts: attempt }),
                Err(e) => {
                    self.log(&format!("[!] attempt {}/{}: {}", attempt, budget, e));
                    last_err = Some(e);
                }
            }
        }

        match repeated {
            Some(ip) => Ok(RotationOutcome::SameIp { ip, attempts: budget }),
            None => Err(last_err.unwrap_or_else(|| "rotation failed".to_string())),
        }
    }

    /// Run full offline diagnostics for modem, SIM card, RF metrics, and network state.
    pub fn run_diagnostics(&self) -> DiagnosticReport {
        let mut report = DiagnosticReport {
            host: self.base_url.clone(),
            reachable: false,
            authenticated: false,
            auth_error: None,
            login_lock_seconds: 0,
            hardware_version: String::new(),
            firmware_version: String::new(),
            imei: String::new(),
            modem_sn: String::new(),
            battery: String::new(),
            wifi_devices: String::new(),
            sim_detected: false,
            sim_state: String::new(),
            pin_status: String::new(),
            puk_status: String::new(),
            iccid: String::new(),
            imsi: String::new(),
            registered: false,
            network_type: String::new(),
            provider: String::new(),
            roaming: false,
            band: String::new(),
            channel: String::new(),
            pci: String::new(),
            cell_id: String::new(),
            band_lock: String::new(),
            rsrp: String::new(),
            rssi: String::new(),
            rsrq: String::new(),
            sinr: String::new(),
            ppp_status: String::new(),
            wan_ip: String::new(),
            dial_mode: String::new(),
            apn: String::new(),
            findings: Vec::new(),
            recommendations: Vec::new(),
        };

        // Query 1: Basic pre-auth query
        let pre_keys = "wa_inner_version,cr_version,hardware_version,modem_msn,imei,m_imei,\
                        sim_card_status,sim_state,pin_status,puk_status,sim_save_pin_status,\
                        login_lock_time,loginfo,network_type,network_provider,strFullName,strShortName,\
                        ppp_status,battery_value,battery_status,wifi_access_device_num,roam_state,roaming";

        let pre_map = match self.get_cmd(pre_keys, true) {
            Ok(m) => {
                report.reachable = true;
                m
            }
            Err(_) => {
                match self.get_cmd("wa_inner_version", false) {
                    Ok(m) => {
                        report.reachable = true;
                        m
                    }
                    Err(e) => {
                        report.findings.push(format!("Cannot reach modem at {}: {}", self.base_url, e));
                        report.recommendations.push(format!("Verify USB cable / RNDIS network adapter is connected and assigned an IP on {}", self.base_url));
                        return report;
                    }
                }
            }
        };

        let g = |m: &HashMap<String, serde_json::Value>, ks: &[&str]| get_first_non_empty(m, ks, "").to_string();

        report.firmware_version = g(&pre_map, &["wa_inner_version", "cr_version"]);
        report.hardware_version = g(&pre_map, &["hardware_version"]);
        report.imei = g(&pre_map, &["imei", "m_imei"]);
        report.modem_sn = g(&pre_map, &["modem_msn"]);
        report.battery = g(&pre_map, &["battery_value"]);
        report.wifi_devices = g(&pre_map, &["wifi_access_device_num"]);

        let lock_str = g(&pre_map, &["login_lock_time"]);
        if let Ok(secs) = lock_str.trim().parse::<u64>() {
            report.login_lock_seconds = secs;
        }

        let sim_st = g(&pre_map, &["sim_card_status", "sim_state"]);
        report.sim_state = sim_st.clone();
        report.pin_status = g(&pre_map, &["pin_status"]);
        report.puk_status = g(&pre_map, &["puk_status"]);

        let sim_lower = sim_st.to_lowercase();
        report.sim_detected = !sim_st.is_empty()
            && !sim_lower.contains("no_sim")
            && !sim_lower.contains("error")
            && !sim_lower.contains("none")
            && sim_st != "0";

        report.network_type = g(&pre_map, &["network_type"]);
        report.provider = g(&pre_map, &["network_provider", "strFullName", "strShortName"]);
        report.ppp_status = g(&pre_map, &["ppp_status"]);
        let roam_str = g(&pre_map, &["roam_state", "roaming"]).to_lowercase();
        report.roaming = roam_str == "1" || roam_str == "roam" || roam_str == "true";

        if report.login_lock_seconds > 0 {
            report.findings.push(format!("WebUI login is currently locked (~{}s remaining). Too many failed password attempts.", report.login_lock_seconds));
            report.recommendations.push("Wait until lockout expires before attempting login.".to_string());
        } else if !self.password.is_empty() {
            match self.ensure_logged_in() {
                Ok(()) => {
                    report.authenticated = true;
                }
                Err(e) => {
                    report.auth_error = Some(e.clone());
                    report.findings.push(format!("WebUI authentication failed: {}", e));
                    report.recommendations.push("Check that the configured admin password is correct.".to_string());
                }
            }
        } else {
            report.findings.push("No admin password provided (running in unauthenticated/read-only mode).".to_string());
        }

        let post_keys = "wan_ipaddr,ipv6_wan_ipaddr,wan_active_band,wan_active_channel,lte_earfcn,\
                         lte_pci,cell_id,network_cell_id,network_lte_rsrp,lte_rsrp,lte_rsrq,lte_snr,\
                         network_sinr,lte_rssi,rscp,ecio,lte_band_lock,dial_mode,m_dial_mode,auto_dial,\
                         apn_name,m_apn_name,profile_name,pdp_type,iccid,sim_imsi";

        if let Ok(post_map) = self.get_cmd(post_keys, true) {
            report.wan_ip = g(&post_map, &["wan_ipaddr", "ipv6_wan_ipaddr"]);
            report.band = g(&post_map, &["wan_active_band"]);
            report.channel = g(&post_map, &["wan_active_channel", "lte_earfcn"]);
            report.pci = g(&post_map, &["lte_pci"]);
            report.cell_id = g(&post_map, &["cell_id", "network_cell_id"]);
            report.rsrp = g(&post_map, &["network_lte_rsrp", "lte_rsrp", "rscp"]);
            report.rssi = g(&post_map, &["lte_rssi"]);
            report.rsrq = g(&post_map, &["lte_rsrq", "ecio"]);
            report.sinr = g(&post_map, &["network_sinr", "lte_snr"]);

            let raw_band_lock = g(&post_map, &["lte_band_lock"]);
            report.band_lock = if !raw_band_lock.is_empty() {
                decode_bands(&raw_band_lock)
            } else {
                "Auto".to_string()
            };

            report.dial_mode = g(&post_map, &["dial_mode", "m_dial_mode", "auto_dial"]);
            report.apn = g(&post_map, &["apn_name", "m_apn_name", "profile_name"]);
            report.iccid = g(&post_map, &["iccid"]);
            report.imsi = g(&post_map, &["sim_imsi"]);
        }

        let nt = report.network_type.to_uppercase();
        report.registered = !nt.is_empty() && nt != "NO_SERVICE" && !nt.starts_with("LIMITED_SERVICE") && nt != "NONE";

        if !report.sim_detected {
            report.findings.push("SIM card is NOT detected by the modem slot (NO_SIM / SIM_ERROR).".to_string());
            report.recommendations.push("Power off modem, remove SIM card, clean contacts, re-insert firmly, and power on.".to_string());
        } else {
            let pin_up = report.pin_status.to_uppercase();
            if pin_up.contains("CHECK_PIN") || pin_up.contains("PIN_REQUIRED") || pin_up == "1" {
                report.findings.push("SIM PIN lock is active on this SIM card.".to_string());
                report.recommendations.push("Disable or enter the SIM PIN in the WebUI (Settings -> Device Settings -> PIN Management).".to_string());
            }
            let puk_up = report.puk_status.to_uppercase();
            if puk_up.contains("CHECK_PUK") || puk_up.contains("PUK_REQUIRED") {
                report.findings.push("SIM card is PUK locked.".to_string());
                report.recommendations.push("Enter the carrier PUK code via the WebUI to unlock the SIM.".to_string());
            }
        }

        if report.sim_detected && !report.registered {
            report.findings.push(format!("Modem is in '{}' state (no cellular network registration).", if report.network_type.is_empty() { "NO_SERVICE" } else { &report.network_type }));
            if report.band_lock != "Auto" && report.band_lock != "All Bands (Auto)" && report.band_lock != "None" {
                report.findings.push(format!("Active band lock restriction: {}. The carrier might not have coverage on this specific band.", report.band_lock));
                report.recommendations.push("Run `zte-control unlock-bands` to re-enable all bands (2G/3G + all LTE).".to_string());
            } else {
                report.recommendations.push("Check antenna / signal coverage in your location, or verify SIM subscription is active with carrier.".to_string());
            }
        }

        if report.registered {
            if report.ppp_status != "ppp_connected" {
                report.findings.push(format!("Modem is registered on network ({}) but data bearer is disconnected (ppp_status: {}).", report.provider, if report.ppp_status.is_empty() { "disconnected" } else { &report.ppp_status }));
                let dm = report.dial_mode.to_lowercase();
                if dm.contains("manual") {
                    report.findings.push("Connection dial mode is set to 'manual' instead of 'auto'.".to_string());
                    report.recommendations.push("Switch dial mode to Auto in WebUI or trigger `zte-control reconnect`.".to_string());
                }
                if report.apn.is_empty() {
                    report.findings.push("No APN (Access Point Name) profile detected for this SIM.".to_string());
                    report.recommendations.push("Configure carrier APN under WebUI -> APN Settings.".to_string());
                }
                report.recommendations.push("Run `zte-control reconnect` to force cellular data bearer connection.".to_string());
            } else if report.wan_ip.is_empty() {
                report.findings.push("Data bearer is connected but no WAN IPv4 address was assigned by the carrier.".to_string());
            }
        }

        if let Ok(rsrp_val) = report.rsrp.trim().trim_end_matches("dBm").trim().parse::<i32>() {
            if rsrp_val < -115 && rsrp_val > -150 {
                report.findings.push(format!("Cellular signal is very weak (RSRP: {} dBm). Connection may be unstable.", rsrp_val));
                report.recommendations.push("Reposition modem closer to a window or higher location.".to_string());
            }
        }

        report
    }
}

/// What a rotation actually achieved. The three cases are kept distinct on
/// purpose: "the bearer came back" and "the public address changed" are not the
/// same claim, and callers (REST API, GUI, fleet log) must not conflate them.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum RotationOutcome {
    /// Verified: the bearer returned with an address different from the old one.
    NewIp {
        ip: String,
        previous: Option<String>,
        attempts: u32,
    },
    /// The bearer returned, but the carrier re-issued the same address every hop.
    SameIp { ip: String, attempts: u32 },
    /// The bearer returned; its address could not be read (no web session), so
    /// whether the address changed is unknown.
    BearerUpIpUnknown { attempts: u32 },
}

impl RotationOutcome {
    /// The post-rotation address, when one could be read.
    pub fn ip(&self) -> Option<&str> {
        match self {
            RotationOutcome::NewIp { ip, .. } | RotationOutcome::SameIp { ip, .. } => Some(ip),
            RotationOutcome::BearerUpIpUnknown { .. } => None,
        }
    }

    /// True only when the address is known to have changed.
    pub fn verified(&self) -> bool {
        matches!(self, RotationOutcome::NewIp { .. })
    }

    pub fn summary(&self) -> String {
        match self {
            RotationOutcome::NewIp { ip, previous, attempts } => format!(
                "new WAN IP {} (was {}) after {} band-hop(s)",
                ip,
                previous.as_deref().unwrap_or("unknown"),
                attempts
            ),
            RotationOutcome::SameIp { ip, attempts } => format!(
                "bearer reset {} time(s) but the carrier kept the same WAN IP {}",
                attempts, ip
            ),
            RotationOutcome::BearerUpIpUnknown { attempts } => format!(
                "bearer reconnected after {} hop(s); WAN IP unreadable (log in to verify the address changed)",
                attempts
            ),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DiagnosticReport {
    pub host: String,
    pub reachable: bool,
    pub authenticated: bool,
    pub auth_error: Option<String>,
    pub login_lock_seconds: u64,

    pub hardware_version: String,
    pub firmware_version: String,
    pub imei: String,
    pub modem_sn: String,
    pub battery: String,
    pub wifi_devices: String,

    pub sim_detected: bool,
    pub sim_state: String,
    pub pin_status: String,
    pub puk_status: String,
    pub iccid: String,
    pub imsi: String,

    pub registered: bool,
    pub network_type: String,
    pub provider: String,
    pub roaming: bool,
    pub band: String,
    pub channel: String,
    pub pci: String,
    pub cell_id: String,
    pub band_lock: String,
    pub rsrp: String,
    pub rssi: String,
    pub rsrq: String,
    pub sinr: String,

    pub ppp_status: String,
    pub wan_ip: String,
    pub dial_mode: String,
    pub apn: String,

    pub findings: Vec<String>,
    pub recommendations: Vec<String>,
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
        if mask == u64::MAX {
            return "All Bands (Auto)".to_string();
        }
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
        if let Some(s) = map.get(*k).and_then(|v| v.as_str()) {
            if !s.trim().is_empty() && s != "None" {
                return s;
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
    probe_client: Option<Client>,
}

impl FleetModem {
    fn new(cfg: ModemConfig, log: Logger) -> Self {
        let client = ZTEClient::new(&cfg.host, &cfg.password, Some(&cfg.bind_ip));
        client.set_logger(log);
        let probe_client = cfg.bind_ip.parse::<IpAddr>().ok().and_then(|ip| {
            Client::builder()
                .local_address(ip)
                .timeout(Duration::from_secs(4))
                .build()
                .ok()
        });
        Self {
            cfg,
            client,
            probe_client,
        }
    }

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
        match (probe, &self.probe_client) {
            (Some(url), Some(client)) => match client.get(url).send() {
                Ok(r) => r.status().is_success() || r.status().is_redirection(),
                Err(_) => false,
            },
            (Some(url), None) => probe_through(&self.cfg.bind_ip, url),
            (None, _) => true,
        }
    }
}

/// Fallback helper to issue an HTTP GET bound to `bind_ip`.
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

/// Convenience sink for callers that just want the progress on stdout.
pub fn stdout_logger() -> Logger {
    Arc::new(|m: &str| println!("{}", m))
}

/// Run the make-before-break ping-pong. `log` receives every progress line --
/// GUI callers pass a sink that forwards into their own log view, since a
/// windowed build has no console for `println!` to reach.
pub fn fleet_rotate(cfg: FleetConfig, once: bool, log: Logger) -> Result<(), String> {
    if cfg.modems.len() < 2 {
        return Err("fleet-rotate needs at least 2 modems in the config".to_string());
    }
    let probe = cfg.probe_url.as_deref();
    let modems: Vec<FleetModem> = cfg
        .modems
        .into_iter()
        .map(|m| FleetModem::new(m, Arc::clone(&log)))
        .collect();

    // Pick an initial ACTIVE that is already solid.
    let mut active = (0..modems.len())
        .find(|&i| modems[i].is_solid(probe))
        .ok_or("no modem has a solid connection to start from")?;
    apply_active(&modems, active)?;
    log(&format!("[fleet] ACTIVE = {}", modems[active].cfg.name));

    loop {
        // Rotate the next non-active modem (round-robin for N > 2).
        let standby = (active + 1) % modems.len();
        log(&format!("[fleet] rotating STANDBY = {}", modems[standby].cfg.name));
        match modems[standby].rotate() {
            Ok(outcome) => log(&format!(
                "[fleet]   {}: {}",
                modems[standby].cfg.name,
                outcome.summary()
            )),
            Err(e) => log(&format!(
                "[fleet]   rotate error on {}: {}",
                modems[standby].cfg.name, e
            )),
        }

        if wait_until_solid(&modems[standby], probe, cfg.solid_timeout_seconds) {
            // Make-before-break: the new path is up before we drop the old one.
            apply_active(&modems, standby)?;
            active = standby;
            log(&format!("[fleet] SWAPPED -> ACTIVE = {}", modems[active].cfg.name));
        } else {
            log(&format!(
                "[fleet] {} not solid after rotate; keeping ACTIVE = {}",
                modems[standby].cfg.name, modems[active].cfg.name
            ));
        }

        if once {
            break;
        }

        // Dwell, watching the active modem; bail early if it drops. `dropped`
        // carries the verdict out of the loop so the emergency check below does
        // not re-probe the modem it just probed.
        let mut waited = 0u64;
        let mut dropped = false;
        while waited < cfg.dwell_seconds {
            thread::sleep(Duration::from_secs(3));
            waited += 3;
            if !modems[active].is_solid(probe) {
                dropped = true;
                break;
            }
        }

        // Emergency: active lost its bearer -> swap to any solid peer immediately.
        if dropped {
            if let Some(peer) = first_solid_other(&modems, active, probe) {
                apply_active(&modems, peer)?;
                log(&format!(
                    "[fleet] EMERGENCY swap -> ACTIVE = {} (previous active lost bearer)",
                    modems[peer].cfg.name
                ));
                active = peer;
            } else {
                log("[fleet] WARNING: active lost bearer and no solid peer to swap to");
            }
        }
    }
    Ok(())
}

impl FleetModem {
    fn rotate(&self) -> Result<RotationOutcome, String> {
        self.client.rotate_and_reconnect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha256_hex_upper() {
        let hash = ZTEClient::sha256_hex_upper("admin");
        assert_eq!(
            hash,
            "8C6976E5B5410415BDE908BD4DEE15DFB167A9C873FC4BB8A81F6F2AB448A918"
        );
    }

    #[test]
    fn test_md5_hex_lower() {
        let hash = ZTEClient::md5_hex_lower("admin");
        assert_eq!(hash, "21232f297a57a5a743894a0e4a801fc3");
    }

    #[test]
    fn test_decode_bands() {
        assert_eq!(decode_bands("0x0000000000000004"), "B3 (1800)");
        assert_eq!(decode_bands("0x0000000000000040"), "B7 (2600)");
        assert_eq!(decode_bands("0x0000000000000080"), "B8 (900)");
        assert_eq!(decode_bands("0x0000000000080000"), "B20 (800)");
        assert_eq!(decode_bands("0x00000000000800c4"), "B3 (1800), B7 (2600), B8 (900), B20 (800)");
        assert_eq!(decode_bands("0xffffffffffffffff"), "All Bands (Auto)");
        assert_eq!(decode_bands("invalid_hex"), "Auto");
    }

    #[test]
    fn test_get_first_non_empty() {
        let mut map = HashMap::new();
        map.insert("empty".to_string(), serde_json::json!(""));
        map.insert("none_str".to_string(), serde_json::json!("None"));
        map.insert("valid".to_string(), serde_json::json!("10.0.0.1"));

        assert_eq!(get_first_non_empty(&map, &["empty", "none_str", "valid"], "def"), "10.0.0.1");
        assert_eq!(get_first_non_empty(&map, &["empty", "none_str"], "def"), "def");
    }

    #[test]
    fn test_fleet_config_deserialization() {
        let json_data = r#"{
            "dwell_seconds": 30,
            "solid_timeout_seconds": 45,
            "probe_url": "http://1.1.1.1",
            "modems": [
                {
                    "name": "m1",
                    "host": "http://192.168.0.1",
                    "password": "pass",
                    "bind_ip": "192.168.0.10"
                },
                {
                    "name": "m2",
                    "host": "http://192.168.8.1",
                    "password": "pass",
                    "bind_ip": "192.168.8.10"
                }
            ]
        }"#;
        let cfg: FleetConfig = serde_json::from_str(json_data).expect("deserialize failed");
        assert_eq!(cfg.dwell_seconds, 30);
        assert_eq!(cfg.solid_timeout_seconds, 45);
        assert_eq!(cfg.probe_url, Some("http://1.1.1.1".to_string()));
        assert_eq!(cfg.modems.len(), 2);
        assert_eq!(cfg.modems[0].name, "m1");
        assert_eq!(cfg.modems[1].name, "m2");
    }

    #[test]
    fn test_zte_client_init() {
        let client = ZTEClient::new("http://192.168.8.1/", "password", Some("127.0.0.1"));
        assert_eq!(client.base_url, "http://192.168.8.1");
        assert_eq!(client.password, "password");
        assert!(client.session_cookie().is_none());
    }

    #[test]
    fn test_debug_does_not_leak_password() {
        let client = ZTEClient::new("http://192.168.8.1", "hunter2", None);
        let rendered = format!("{:?}", client);
        assert!(!rendered.contains("hunter2"), "password leaked: {}", rendered);
        assert!(rendered.contains("192.168.8.1"));
    }

    #[test]
    fn test_band_cycle_is_per_client() {
        // Two clients (e.g. two fleet modems) must each start at the beginning of
        // the band list rather than sharing one global cursor.
        let a = ZTEClient::new("http://192.168.0.1", "", None);
        let b = ZTEClient::new("http://192.168.8.1", "", None);
        a.band_cycle.fetch_add(3, Ordering::SeqCst);
        assert_eq!(a.band_cycle.load(Ordering::SeqCst), 3);
        assert_eq!(b.band_cycle.load(Ordering::SeqCst), 0);

        // ...but clones of one client share it, so a cloned handle keeps hopping
        // instead of restarting on the same band.
        let a2 = a.clone();
        a2.band_cycle.fetch_add(1, Ordering::SeqCst);
        assert_eq!(a.band_cycle.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn test_logger_redirects_progress() {
        let sink = Arc::new(Mutex::new(Vec::<String>::new()));
        let client = ZTEClient::new("http://192.168.8.1", "", None);
        let captured = Arc::clone(&sink);
        client.set_logger(Arc::new(move |m: &str| {
            captured.lock().unwrap().push(m.to_string());
        }));
        client.log("hello");
        assert_eq!(sink.lock().unwrap().as_slice(), ["hello"]);
    }

    #[test]
    fn test_rotation_outcome_reporting() {
        let new_ip = RotationOutcome::NewIp {
            ip: "10.1.2.3".to_string(),
            previous: Some("10.9.9.9".to_string()),
            attempts: 2,
        };
        assert!(new_ip.verified());
        assert_eq!(new_ip.ip(), Some("10.1.2.3"));
        assert!(new_ip.summary().contains("10.9.9.9"));

        let same = RotationOutcome::SameIp { ip: "10.1.2.3".to_string(), attempts: 3 };
        assert!(!same.verified());
        assert_eq!(same.ip(), Some("10.1.2.3"));

        // The case that used to be reported as a successful rotation with the
        // literal IP "connected".
        let unknown = RotationOutcome::BearerUpIpUnknown { attempts: 1 };
        assert!(!unknown.verified());
        assert_eq!(unknown.ip(), None);
    }

    #[test]
    fn test_rotation_outcome_serializes_tagged() {
        let json = serde_json::to_value(RotationOutcome::SameIp {
            ip: "1.2.3.4".to_string(),
            attempts: 3,
        })
        .expect("serialize");
        assert_eq!(json["result"], "same_ip");
        assert_eq!(json["ip"], "1.2.3.4");
    }
}

