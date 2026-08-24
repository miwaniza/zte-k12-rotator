//! zte-control core: modem WebUI client, band/bearer control, and multi-modem
//! fleet rotation. Shared by the CLI (`main.rs`) and the egui desktop app
//! (`zte-egui`).

use base64::Engine as _;
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, TryLockError};
use std::thread;
use std::time::{Duration, Instant};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// How many band-hops `rotate_and_reconnect` will try before giving up on getting
/// an address different from the one the modem held beforehand.
pub const DEFAULT_ROTATE_ATTEMPTS: u32 = 3;

// ---------------------------------------------------------------------------
// Timing budgets
//
// These are WALL-CLOCK deadlines, not iteration counts. Counting iterations
// (`for _ in 0..tries { sleep(1); http_get(); }`) ignores the request itself, so
// against an unresponsive modem -- where every GET burns the full HTTP_TIMEOUT --
// a "20 second" wait actually ran for over two minutes.
// ---------------------------------------------------------------------------

/// Per-request HTTP timeout for modem control traffic.
const HTTP_TIMEOUT: Duration = Duration::from_secs(6);
/// Gap between polls while waiting for the modem to reach a state.
const POLL_INTERVAL: Duration = Duration::from_secs(1);
/// How long to wait for the bearer after a band change. Re-registration on a new
/// band takes ~10-20s (measured), so this leaves headroom.
const BEARER_TIMEOUT: Duration = Duration::from_secs(25);
/// How long to wait for the modem to register on *any* RAT during the heal path.
const REGISTER_TIMEOUT: Duration = Duration::from_secs(30);
/// Settle time after DISCONNECT so the PGW drops the old IP lease.
const BEARER_DROP_SETTLE: Duration = Duration::from_millis(1600);

/// Progress sink. Long operations (rotation, fleet cycles) report through this so
/// a GUI -- which has no stdout to print to -- can show what is happening.
pub type Logger = Arc<dyn Fn(&str) + Send + Sync>;

/// The fields a status view needs. Declared once: this list was previously
/// spelled out separately in `get_status`, in the diagnostics, and in the egui
/// backend, and the copies had already drifted apart.
pub const STATUS_KEYS: &str = "wa_inner_version,hardware_version,imei,network_provider,network_type,\
     network_lte_rsrp,lte_rsrp,lte_rsrq,lte_snr,network_sinr,lte_rssi,lte_band_lock,wan_active_band,\
     wan_active_channel,lte_earfcn,lte_pci,cell_id,network_cell_id,wan_ipaddr,ppp_status,\
     strFullName,strShortName";

/// Bands the rotation walks, in order.
///
/// "All bands" is deliberately NOT a step here: it is the recovery state (see
/// `unlock_bands`), and hopping to it does not constrain the radio at all, so a
/// retry that landed on it was unlikely to move the address.
const ROTATION_MASKS: &[(&str, &str)] = &[
    ("Band 8 (900 MHz)",  "0x0000000000000080"),
    ("Band 3 (1800 MHz)", "0x0000000000000004"),
    ("Band 7 (2600 MHz)", "0x0000000000000040"),
    ("Band 20 (800 MHz)", "0x0000000000080000"),
];

/// PPP authentication for an APN profile. Most carriers, Kyivstar included, need
/// none; PAP/CHAP take a username and password.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApnAuth {
    None,
    Pap { username: String, password: String },
    Chap { username: String, password: String },
}

impl ApnAuth {
    pub fn mode(&self) -> &'static str {
        match self {
            ApnAuth::None => "none",
            ApnAuth::Pap { .. } => "pap",
            ApnAuth::Chap { .. } => "chap",
        }
    }
    pub fn username(&self) -> &str {
        match self {
            ApnAuth::None => "",
            ApnAuth::Pap { username, .. } | ApnAuth::Chap { username, .. } => username,
        }
    }
    pub fn password(&self) -> &str {
        match self {
            ApnAuth::None => "",
            ApnAuth::Pap { password, .. } | ApnAuth::Chap { password, .. } => password,
        }
    }
}

/// What went wrong. Replaces the `String` errors this crate used to return, which
/// callers could only inspect by matching on message text.
#[derive(Debug)]
pub enum Error {
    /// The request never completed (cable out, wrong host, timeout).
    Transport { context: &'static str, source: reqwest::Error },
    /// The modem answered with something that is not the JSON we expect.
    Decode { context: &'static str, source: reqwest::Error },
    /// Something answered, but it is not a ZTE WebUI. Usually another device
    /// squatting the same address -- a home router on 192.168.0.1 and a
    /// factory-reset modem on 192.168.0.1 are indistinguishable by address alone.
    NotZteWebui { host: String, detail: String },
    /// A SET command needs a password and none is configured.
    NoPassword,
    /// The modem rejected the credentials.
    AuthFailed(String),
    /// The modem accepted the request (HTTP 200) but reported failure in the body.
    CommandRejected { goform_id: String, result: String },
    /// Another rotation is already running against this modem.
    RotationBusy,
    /// The bearer never came back.
    RotationFailed(String),
    /// Bad fleet configuration, or a routing command that failed.
    Config(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Transport { context, source } => write!(f, "{}: {}", context, source),
            Error::Decode { context, source } => write!(f, "{}: malformed response ({})", context, source),
            Error::NotZteWebui { host, detail } => write!(
                f,
                "{} answered, but it is not a ZTE WebUI ({}). Another device is probably using this address — check which interface the request went out of",
                host, detail
            ),
            Error::NoPassword => write!(f, "no password configured for WebUI authentication"),
            Error::AuthFailed(m) => write!(f, "authentication failed: {}", m),
            Error::CommandRejected { goform_id, result } => {
                write!(f, "modem rejected {}: result={}", goform_id, result)
            }
            Error::RotationBusy => write!(f, "a rotation is already in progress on this modem"),
            Error::RotationFailed(m) => write!(f, "rotation failed: {}", m),
            Error::Config(m) => write!(f, "{}", m),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Transport { source, .. } | Error::Decode { source, .. } => Some(source),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// "All bands" masks, passed with `is_*_band=1` to re-enable every 2G/3G ("gw")
/// and LTE band so the modem always has a usable RAT to camp on.
const GW_BAND_ALL: &str = "0xffffffffffffffff";
const LTE_BAND_ALL: &str = "0xffffffffffffffff";

#[derive(Clone)]
pub struct ZTEClient {
    pub base_url: String,
    /// Private: a public field handed the WebUI password to every consumer of the
    /// crate. `has_password` is all callers actually needed.
    password: String,
    /// Private: `main.rs`'s dashboard proxy used to reach in here and build its
    /// own requests, which put session handling outside the type that owns the
    /// session. It goes through `forward_get` / `forward_post` instead.
    client: Client,
    /// Captured session cookie (e.g. `stok=...`) replayed on every request.
    /// The MF920U sets a malformed cookie (`HttpOnly=1`, invalid `Expires`) that
    /// reqwest's cookie store silently drops, so we carry it manually instead.
    session_cookie: Arc<Mutex<Option<String>>>,
    firmware_version_cache: Arc<Mutex<Option<String>>>,
    /// Per-client cursor into `ROTATION_MASKS`. This used to be a process-global
    /// static, which made two modems in a fleet interleave one shared cycle
    /// instead of each walking the band list independently.
    band_cycle: Arc<AtomicUsize>,
    /// Held for the duration of a rotation. Two concurrent rotations against one
    /// modem would interleave their DISCONNECT/CONNECT/BAND_SELECT commands and
    /// each other's band cursor -- reachable today from the tray button, a
    /// scheduled script and the dashboard at the same time.
    rotation_guard: Arc<Mutex<()>>,
    /// Where progress messages go; `println!` when unset (the CLI default).
    /// Shared with clones, immutable after construction -- so reporting a line
    /// costs no lock.
    logger: Option<Logger>,
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
            .timeout(HTTP_TIMEOUT);

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
            rotation_guard: Arc::new(Mutex::new(())),
            logger: None,
        }
    }

    /// Route this client's progress messages somewhere other than stdout.
    pub fn with_logger(mut self, sink: Logger) -> Self {
        self.logger = Some(sink);
        self
    }

    /// True when a password is configured, i.e. SET commands are possible.
    pub fn has_password(&self) -> bool {
        !self.password.is_empty()
    }

    fn log(&self, msg: &str) {
        match &self.logger {
            Some(f) => f(msg),
            None => println!("{}", msg),
        }
    }

    /// The current session cookie pair (`name=value`), if logged in.
    pub fn session_cookie(&self) -> Option<String> {
        self.session_cookie.lock().ok().and_then(|g| g.as_ref().cloned())
    }

    /// Capture the session cookie from a login response's `Set-Cookie` header(s).
    ///
    /// Prefers a known session cookie name and only falls back to "the first pair
    /// we saw". Taking the first pair unconditionally meant that any unrelated
    /// cookie emitted ahead of the session one (a language or theme preference)
    /// would be captured instead, after which every SET failed with no clue why.
    pub fn capture_session_cookie(&self, resp: &reqwest::blocking::Response) {
        const SESSION_NAMES: &[&str] = &["stok", "sessionid", "jsessionid", "sid", "_sid"];

        let mut fallback: Option<String> = None;
        for hv in resp.headers().get_all(reqwest::header::SET_COOKIE).iter() {
            let Ok(s) = hv.to_str() else { continue };
            let Some(pair) = s.split(';').next().map(|p| p.trim()) else { continue };
            let Some((name, _)) = pair.split_once('=') else { continue };

            if SESSION_NAMES.iter().any(|n| name.eq_ignore_ascii_case(n)) {
                self.store_cookie(pair);
                return;
            }
            fallback.get_or_insert_with(|| pair.to_string());
        }
        if let Some(pair) = fallback {
            self.store_cookie(&pair);
        }
    }

    fn store_cookie(&self, pair: &str) {
        if let Ok(mut g) = self.session_cookie.lock() {
            *g = Some(pair.to_string());
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

    /// A GET against the modem WebUI carrying our session, headers and Referer.
    fn get_request(&self, url: &str) -> reqwest::blocking::RequestBuilder {
        let mut req = self
            .client
            .get(url)
            .header("X-Requested-With", "XMLHttpRequest")
            .header("Referer", format!("{}/index.html", self.base_url));
        if let Some(cookie) = self.session_cookie() {
            req = req.header("Cookie", cookie);
        }
        req
    }

    /// As `get_request`, for form POSTs.
    fn post_request(&self, url: &str) -> reqwest::blocking::RequestBuilder {
        let mut req = self
            .client
            .post(url)
            .header("Content-Type", "application/x-www-form-urlencoded; charset=UTF-8")
            .header("X-Requested-With", "XMLHttpRequest")
            .header("Referer", format!("{}/index.html", self.base_url));
        if let Some(cookie) = self.session_cookie() {
            req = req.header("Cookie", cookie);
        }
        req
    }

    pub fn get_cmd(&self, cmd: &str, multi: bool) -> Result<HashMap<String, serde_json::Value>> {
        let multi_flag = if multi { "&multi_data=1" } else { "" };
        let url = format!(
            "{}/goform/goform_get_cmd_process?cmd={}{}&isTest=false&_={}",
            self.base_url,
            cmd,
            multi_flag,
            chrono_ms()
        );

        let resp = self.get_request(&url).send().map_err(|e| Error::Transport {
            context: "modem GET",
            source: e,
        })?;

        // Read the body first so a non-JSON answer can be identified rather than
        // reported as a generic decode failure. An HTML page here means some other
        // device is on this address -- the case that actually happens is a home
        // router and a factory-reset modem both sitting on 192.168.0.1.
        let body = resp.text().map_err(|e| Error::Transport {
            context: "modem GET",
            source: e,
        })?;

        serde_json::from_str(&body).map_err(|e| {
            let head = body.trim_start();
            if head.starts_with('<') || head.to_ascii_lowercase().starts_with("<!doctype") {
                Error::NotZteWebui {
                    host: self.base_url.clone(),
                    detail: "responded with an HTML page instead of JSON".to_string(),
                }
            } else {
                Error::NotZteWebui {
                    host: self.base_url.clone(),
                    detail: format!("unparseable response: {}", e),
                }
            }
        })
    }

    /// Forward a raw `/goform/...` GET and return the body verbatim. Used by the
    /// dashboard proxy so that session handling stays inside this type.
    pub fn forward_get(&self, path: &str) -> Result<String> {
        let url = format!("{}{}", self.base_url, path);
        self.get_request(&url)
            .send()
            .and_then(|r| r.text())
            .map_err(|e| Error::Transport { context: "proxy GET", source: e })
    }

    /// Forward a raw `/goform/...` form POST. Captures any session cookie the
    /// modem hands back, so a login performed through the proxy authenticates
    /// this client too.
    pub fn forward_post(&self, path: &str, body: Vec<u8>) -> Result<String> {
        let url = format!("{}{}", self.base_url, path);
        let resp = self
            .post_request(&url)
            .body(body)
            .send()
            .map_err(|e| Error::Transport { context: "proxy POST", source: e })?;
        self.capture_session_cookie(&resp);
        resp.text().map_err(|e| Error::Transport { context: "proxy POST", source: e })
    }

    /// Request authentication token (AD) required by sensitive SET commands. Both firmware families
    /// hash the live `wa_inner_version` against a fresh per-request `RD`, but differ
    /// in algorithm:
    ///   * K12/ZX297520: `AD = sha256_upper( sha256_upper(wa_inner_version) + RD )`
    ///   * MF920U-class: `AD = md5_lower( md5_lower(wa_inner_version + cr_version) + RD )`
    ///
    /// The version is read live from the device rather than hardcoded, so a firmware
    /// revision bump does not silently break the token.
    pub fn get_ad_token(&self) -> Result<String> {
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

        // `wa` was just read from the device, so answer from it. Calling
        // `is_k12_firmware()` first could miss the firmware cache and spend a
        // whole extra round-trip re-reading the field we are holding.
        let is_k12 = if wa.is_empty() { self.is_k12_firmware() } else { wa.contains("K12") };
        let ad = if is_k12 {
            let fw_hash = Self::sha256_hex_upper(&wa);
            Self::sha256_hex_upper(&format!("{}{}", fw_hash, rd))
        } else {
            let ver_hash = Self::md5_hex_lower(&format!("{}{}", wa, cr));
            Self::md5_hex_lower(&format!("{}{}", ver_hash, rd))
        };
        Ok(ad)
    }

    pub fn login(&self) -> Result<bool> {
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
            .post_request(&url)
            .form(&params)
            .send()
            .map_err(|e| Error::Transport { context: "login POST", source: e })?;

        // Capture the session cookie before the body consumes the response.
        self.capture_session_cookie(&resp);

        let res_map: HashMap<String, serde_json::Value> = resp
            .json()
            .map_err(|e| Error::Decode { context: "login POST", source: e })?;

        // `0` is a plain success. `4` is accepted as "session already established"
        // -- this is inherited behaviour and has NOT been confirmed against ZTE
        // documentation; if a login ever appears to succeed while SETs keep
        // failing, this is the first thing to re-check on real hardware.
        const RESULT_OK: &str = "0";
        const RESULT_ALREADY_LOGGED_IN: &str = "4";
        if let Some(r) = res_map.get("result").and_then(|v| v.as_str()) {
            if r == RESULT_OK || r == RESULT_ALREADY_LOGGED_IN {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn ensure_logged_in(&self) -> Result<()> {
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
        if !self.has_password() {
            return Err(Error::NoPassword);
        }
        if !self.login()? {
            return Err(Error::AuthFailed(
                "modem did not accept the configured password".to_string(),
            ));
        }
        Ok(())
    }

    /// A SET is only successful if the *body* says so.
    ///
    /// The goform endpoint answers HTTP 200 with `{"result":"failure"}` for a
    /// rejected command, so a transport-level `Ok` proves nothing. Nothing in this
    /// crate used to look at the body, which meant a band change refused for a
    /// stale AD token was indistinguishable from one that worked.
    ///
    /// Only an explicitly negative `result` is treated as failure: some commands
    /// answer with an unrelated shape, and rejecting those would invent errors.
    fn check_set_result(
        goform_id: &str,
        body: &HashMap<String, serde_json::Value>,
    ) -> Result<()> {
        let Some(result) = body.get("result") else { return Ok(()) };
        let text = match result {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let failed = matches!(
            text.trim().to_ascii_lowercase().as_str(),
            "failure" | "fail" | "failed" | "error" | "1"
        );
        if failed {
            Err(Error::CommandRejected {
                goform_id: goform_id.to_string(),
                result: text,
            })
        } else {
            Ok(())
        }
    }

    pub fn post_cmd(
        &self,
        goform_id: &str,
        mut params: HashMap<String, String>,
        with_ad: bool,
    ) -> Result<HashMap<String, serde_json::Value>> {
        self.ensure_logged_in()?;

        if with_ad {
            let ad_token = self.get_ad_token()?;
            params.insert("AD".to_string(), ad_token);
        }

        params.insert("isTest".to_string(), "false".to_string());
        params.insert("goformId".to_string(), goform_id.to_string());

        let url = format!("{}/goform/goform_set_cmd_process", self.base_url);
        let resp = self
            .post_request(&url)
            .form(&params)
            .send()
            .map_err(|e| Error::Transport { context: "modem SET", source: e })?;

        let res_map: HashMap<String, serde_json::Value> = resp
            .json()
            .map_err(|e| Error::Decode { context: "modem SET", source: e })?;
        Self::check_set_result(goform_id, &res_map)?;
        Ok(res_map)
    }

    pub fn get_status(&self) -> Result<HashMap<String, serde_json::Value>> {
        let _ = self.ensure_logged_in();
        self.get_cmd(STATUS_KEYS, true)
    }

    /// Apply an LTE band selection while ALWAYS keeping 2G/3G ("gw") bands
    /// enabled as a fallback. Sending `is_gw_band=0, gw_band_mask=0` (the old
    /// behavior) could leave the modem with no usable RAT and strand it in
    /// NO_SERVICE when the chosen LTE band has no coverage.
    fn select_bands(&self, lte_mask: &str) -> Result<HashMap<String, serde_json::Value>> {
        let mut params = HashMap::new();
        params.insert("is_gw_band".to_string(), "1".to_string());
        params.insert("gw_band_mask".to_string(), GW_BAND_ALL.to_string());
        params.insert("is_lte_band".to_string(), "1".to_string());
        params.insert("lte_band_mask".to_string(), lte_mask.to_string());
        self.post_cmd("BAND_SELECT", params, true)
    }

    pub fn lock_bands(&self, bands: &[String]) -> Result<String> {
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
    pub fn unlock_bands(&self) -> Result<String> {
        let _ = self.unlock_cell();
        let res = self.select_bands(LTE_BAND_ALL)?;
        Ok(serde_json::to_string(&res).unwrap_or_default())
    }

    /// Poll until the modem registers on some network (`network_type` present and
    /// not NO_SERVICE/LIMITED_SERVICE). Dialing before this is a no-op, so the
    /// heal path waits for it before issuing CONNECT_NETWORK.
    fn wait_registered(&self, timeout: Duration) -> bool {
        poll_until(timeout, || {
            let st = self.get_cmd("network_type", false).ok()?;
            let nt = st.get("network_type").and_then(|v| v.as_str()).unwrap_or("");
            let registered =
                !nt.is_empty() && nt != "NO_SERVICE" && !nt.starts_with("LIMITED_SERVICE");
            registered.then_some(())
        })
        .is_some()
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
    fn wait_bearer(&self, timeout: Duration) -> Option<Option<String>> {
        poll_until(timeout, || {
            let ppp = self
                .get_cmd("ppp_status", false)
                .ok()
                .and_then(|m| m.get("ppp_status").and_then(|v| v.as_str()).map(|s| s.to_string()))
                .unwrap_or_default();
            (ppp == "ppp_connected").then(|| self.read_wan_ip())
        })
    }

    pub fn lock_cell(&self, earfcn: u32, pci: u32) -> Result<String> {
        let mut params = HashMap::new();
        params.insert("lte_earfcn_lock".to_string(), earfcn.to_string());
        params.insert("lte_pci_lock".to_string(), pci.to_string());

        let res = self.post_cmd("LTE_LOCK_CELL_SET", params, true)?;
        Ok(serde_json::to_string(&res).unwrap_or_default())
    }

    pub fn unlock_cell(&self) -> Result<String> {
        let mut params = HashMap::new();
        params.insert("lte_earfcn_lock".to_string(), "0".to_string());
        params.insert("lte_pci_lock".to_string(), "0".to_string());
        let res = self.post_cmd("LTE_LOCK_CELL_SET", params, true)?;
        Ok(serde_json::to_string(&res).unwrap_or_default())
    }

    /// Dial the data bearer, without touching bands or cell locks.
    ///
    /// Distinct from `rotate_and_reconnect`, which band-hops on the way: a modem
    /// in `manual_dial` that simply has not been told to connect needs a dial,
    /// not a rotation.
    pub fn connect(&self) -> Result<()> {
        let mut params = HashMap::new();
        params.insert("notCallback".to_string(), "true".to_string());
        self.post_cmd("CONNECT_NETWORK", params, true).map(|_| ())
    }

    /// Drop the data bearer.
    pub fn disconnect(&self) -> Result<()> {
        let mut params = HashMap::new();
        params.insert("notCallback".to_string(), "true".to_string());
        self.post_cmd("DISCONNECT_NETWORK", params, true).map(|_| ())
    }

    /// Wait for the bearer, reporting the address if it can be read. Exposed so
    /// `connect` callers can confirm the dial actually took.
    pub fn await_bearer(&self, timeout: Duration) -> Option<Option<String>> {
        self.wait_bearer(timeout)
    }

    /// Set whether the modem dials on its own (`auto`) or waits to be told
    /// (`manual`). A modem left in manual never establishes a bearer after a
    /// power cycle, however healthy the radio is.
    ///
    /// The goform name for this is not in `docs/goform_api_reference.md` and
    /// varies across ZTE firmware. A rejection is reported rather than silently
    /// ignored (see `check_set_result`), so trying it is safe.
    pub fn set_dial_mode(&self, auto: bool) -> Result<()> {
        let mode = if auto { "auto_dial" } else { "manual_dial" };
        let mut params = HashMap::new();
        params.insert("ConnectionMode".to_string(), mode.to_string());
        self.post_cmd("SET_CONNECTION_MODE", params, true).map(|_| ())
    }

    /// Write a manual APN profile and make it the active one.
    ///
    /// `apn_mode=auto` lets the modem pick a profile from its built-in table by
    /// IMSI, which on an MF920U with a Kyivstar SIM selected `www.djuice.com.ua`
    /// -- a sub-brand retired years ago -- and the carrier rejected the PDP
    /// context. Pinning the profile manually is the fix.
    ///
    /// `APN_PROC_EX` is not in `docs/goform_api_reference.md` and its parameter
    /// set varies across ZTE firmware; a rejection is reported rather than
    /// silently ignored (`check_set_result`), so attempting it is safe.
    pub fn set_apn(&self, apn: &str, profile: &str, index: u32, auth: ApnAuth) -> Result<()> {
        let idx = index.to_string();

        let mut set = HashMap::new();
        set.insert("apn_action".to_string(), "set".to_string());
        set.insert("apn_mode".to_string(), "manual".to_string());
        set.insert("profile_name".to_string(), profile.to_string());
        set.insert("wan_apn".to_string(), apn.to_string());
        set.insert("wan_dial".to_string(), "*99#".to_string());
        set.insert("apn_select".to_string(), "manual".to_string());
        set.insert("pdp_type".to_string(), "IP".to_string());
        set.insert("pdp_select".to_string(), "auto".to_string());
        set.insert("index".to_string(), idx.clone());
        set.insert("ppp_auth_mode".to_string(), auth.mode().to_string());
        set.insert("ppp_username".to_string(), auth.username().to_string());
        set.insert("ppp_passwd".to_string(), auth.password().to_string());
        self.post_cmd("APN_PROC_EX", set, true)?;

        // Writing the profile does not select it; without this the modem keeps
        // using whatever it had.
        let mut default = HashMap::new();
        default.insert("apn_action".to_string(), "set_default".to_string());
        default.insert("apn_mode".to_string(), "manual".to_string());
        default.insert("set_default_flag".to_string(), "1".to_string());
        default.insert("pdp_type".to_string(), "IP".to_string());
        default.insert("index".to_string(), idx);
        self.post_cmd("APN_PROC_EX", default, true).map(|_| ())
    }

    /// Point the modem at ONE LTE band, for tower discovery. Goes through
    /// `select_bands`, so 2G/3G stay enabled and an interrupted scan can never
    /// leave the modem with no RAT to camp on.
    pub fn scan_band(&self, lte_mask: &str) -> Result<()> {
        let _ = self.unlock_cell();
        self.select_bands(lte_mask).map(|_| ())
    }

    /// Run a step of a rotation, reporting rather than swallowing a rejection.
    /// These were `let _ = ...`, so a BAND_SELECT the modem refused turned the
    /// "band hop" into a plain bearer bounce with nothing in the log to say so.
    fn rotation_step(&self, what: &str, outcome: Result<impl Sized>) {
        if let Err(e) = outcome {
            self.log(&format!("[!] {} failed: {}", what, e));
        }
    }

    /// One band-hop + carrier bearer reset.
    ///
    /// `Ok(Some(ip))` -- bearer returned and its address was read.
    /// `Ok(None)`     -- bearer returned but the address is unreadable.
    /// `Err(_)`       -- no bearer came back, even after restoring all bands.
    fn rotate_once(&self) -> Result<Option<String>> {
        let idx = self.band_cycle.fetch_add(1, Ordering::SeqCst) % ROTATION_MASKS.len();
        let (band_name, band_mask) = ROTATION_MASKS[idx];
        self.log(&format!("[*] Rotating to frequency {}: mask {}", band_name, band_mask));

        // 1. Clear cell lock
        self.rotation_step("clearing cell lock", self.unlock_cell());

        // 2. Select target LTE band (keeps 2G/3G as fallback via select_bands)
        self.rotation_step(
            &format!("selecting {}", band_name),
            self.select_bands(band_mask),
        );

        // 3. Disconnect cellular session
        let mut p1 = HashMap::new();
        p1.insert("notCallback".to_string(), "true".to_string());
        self.rotation_step("DISCONNECT_NETWORK", self.post_cmd("DISCONNECT_NETWORK", p1, true));

        // 4. Guard sleep so PGW drops old IP lease
        thread::sleep(BEARER_DROP_SETTLE);

        // 5. Connect cellular session
        let mut p2 = HashMap::new();
        p2.insert("notCallback".to_string(), "true".to_string());
        self.rotation_step("CONNECT_NETWORK", self.post_cmd("CONNECT_NETWORK", p2, true));

        // 6. Wait for the bearer on the new band, or a 2G/3G fallback.
        if let Some(ip) = self.wait_bearer(BEARER_TIMEOUT) {
            return Ok(ip);
        }

        // 7. Auto-heal: the target band may have no coverage here. Restore ALL bands
        //    so the modem can re-register on anything (incl. 3G), and give a full
        //    re-scan + register + auto-dial time. Guarantees a rotation can never
        //    strand the modem in NO_SERVICE (which would take it out of the fleet).
        self.log(&format!("[!] bearer did not return on {}; restoring all bands", band_name));
        self.rotation_step("restoring all bands", self.unlock_bands());
        // Register FIRST (on any RAT, incl. 3G), THEN dial -- issuing CONNECT while
        // still NO_SERVICE is a no-op that wastes the window.
        self.wait_registered(REGISTER_TIMEOUT);
        let mut p3 = HashMap::new();
        p3.insert("notCallback".to_string(), "true".to_string());
        self.rotation_step("CONNECT_NETWORK (heal)", self.post_cmd("CONNECT_NETWORK", p3, true));
        match self.wait_bearer(BEARER_TIMEOUT) {
            Some(ip) => Ok(ip),
            None => Err(Error::RotationFailed(
                "no bearer even after restoring all bands".to_string(),
            )),
        }
    }

    /// Band-hop + bearer reset until the carrier hands out an address that differs
    /// from the one held before the rotation, up to `DEFAULT_ROTATE_ATTEMPTS`
    /// hops. Each retry advances to the next band, so a carrier that pins one
    /// address per band still gets away from the old one.
    pub fn rotate_and_reconnect(&self) -> Result<RotationOutcome> {
        self.rotate_verified(DEFAULT_ROTATE_ATTEMPTS)
    }

    /// `rotate_and_reconnect` with an explicit hop budget.
    ///
    /// Refuses to start (`Error::RotationBusy`) while another rotation holds this
    /// modem, rather than interleaving bearer commands with it.
    pub fn rotate_verified(&self, max_attempts: u32) -> Result<RotationOutcome> {
        let _guard = match self.rotation_guard.try_lock() {
            Ok(g) => g,
            Err(TryLockError::WouldBlock) => return Err(Error::RotationBusy),
            // A previous rotation panicked mid-flight. The modem may be in an
            // unknown band state, so heal it rather than refusing forever.
            Err(TryLockError::Poisoned(p)) => p.into_inner(),
        };

        let budget = max_attempts.max(1);
        // The baseline. `None` means we could not read the old address -- no
        // session, or no bearer yet -- NOT that there was no address.
        let previous = self.read_wan_ip();
        let mut repeated: Option<String> = None;
        let mut last_err: Option<Error> = None;

        for attempt in 1..=budget {
            match self.rotate_once() {
                Ok(Some(ip)) => match previous.as_deref() {
                    // No baseline: we have an address but cannot claim it differs
                    // from the old one. Retrying will not produce a baseline
                    // either, so report it plainly instead of looping.
                    None => return Ok(RotationOutcome::UnknownBaseline { ip, attempts: attempt }),
                    Some(prev) if prev != ip => {
                        return Ok(RotationOutcome::NewIp {
                            ip,
                            previous: prev.to_string(),
                            attempts: attempt,
                        })
                    }
                    Some(_) => {
                        self.log(&format!(
                            "[!] attempt {}/{}: carrier re-issued the same address {}; hopping again",
                            attempt, budget, ip
                        ));
                        repeated = Some(ip);
                    }
                },
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
            None => Err(last_err
                .unwrap_or_else(|| Error::RotationFailed("no attempt produced a bearer".into()))),
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
            cell_lock: None,
            rsrp: String::new(),
            rssi: String::new(),
            rsrq: String::new(),
            sinr: String::new(),
            ppp_status: String::new(),
            wan_ip: String::new(),
            dial_mode: String::new(),
            apn: String::new(),
            apn_profile: String::new(),
            apn_mode: String::new(),
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
                        if matches!(e, Error::NotZteWebui { .. }) {
                            report.recommendations.push(format!(
                                "Another device is answering on {}. Find the modem's real address with `Get-NetIPConfiguration`, or bind to its interface with --bind-ip <the adapter's own IP>.",
                                self.base_url
                            ));
                        } else {
                            report.recommendations.push(format!("Verify USB cable / RNDIS network adapter is connected and assigned an IP on {}", self.base_url));
                        }
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

        // What the status field claims, when the firmware reports one at all.
        // `None` = the field is absent, which is NOT the same as "no SIM": the
        // MF920U returns neither `sim_card_status` nor `sim_state`, and treating
        // that silence as absence made the report tell people to clean the
        // contacts of a SIM that was demonstrably working. The verdict is
        // finished below, once the corroborating evidence has been read.
        let sim_status_verdict: Option<bool> = if sim_st.is_empty() {
            None
        } else {
            let sim_lower = sim_st.to_lowercase();
            Some(
                !sim_lower.contains("no_sim")
                    && !sim_lower.contains("error")
                    && !sim_lower.contains("none")
                    && sim_st != "0",
            )
        };

        report.network_type = g(&pre_map, &["network_type"]);
        report.provider = g(&pre_map, &["network_provider", "strFullName", "strShortName"]);
        report.ppp_status = g(&pre_map, &["ppp_status"]);
        let roam_str = g(&pre_map, &["roam_state", "roaming"]).to_lowercase();
        report.roaming = roam_str == "1" || roam_str == "roam" || roam_str == "true";

        // `login_lock_time` is NOT a reliable "you are locked out" signal.
        //
        // Observed on BD_MACTEXPKMF920UV1.0.0B01: it reads ~300 and counts down
        // immediately after a *successful* login, with no failed attempts
        // anywhere in the sequence -- i.e. it behaves as a session timer on this
        // firmware. Refusing to authenticate while it was non-zero meant that one
        // good login silently downgraded every command for the next five minutes
        // to read-only, and the report then blamed the SIM for the auth-gated
        // fields it could no longer read.
        //
        // So: attempt the login and let the *result* decide. Exactly one attempt,
        // so this cannot walk into a real lockout by retrying.
        if self.has_password() {
            match self.ensure_logged_in() {
                Ok(()) => {
                    report.authenticated = true;
                }
                Err(e) => {
                    report.auth_error = Some(e.to_string());
                    if report.login_lock_seconds > 0 {
                        report.findings.push(format!(
                            "WebUI authentication failed and login_lock_time reads ~{}s, so the modem may be rate-limiting logins: {}",
                            report.login_lock_seconds, e
                        ));
                        report.recommendations.push(
                            "Wait for that timer to expire, then retry with the correct password."
                                .to_string(),
                        );
                    } else {
                        report.findings.push(format!("WebUI authentication failed: {}", e));
                        report.recommendations.push("Check that the configured admin password is correct.".to_string());
                    }
                }
            }
        } else {
            report.findings.push("No admin password provided (running in unauthenticated/read-only mode).".to_string());
        }

        let post_keys = "wan_ipaddr,ipv6_wan_ipaddr,wan_active_band,wan_active_channel,lte_earfcn,\
                         lte_pci,cell_id,network_cell_id,network_lte_rsrp,lte_rsrp,lte_rsrq,lte_snr,\
                         network_sinr,lte_rssi,rscp,ecio,lte_band_lock,lte_earfcn_lock,lte_pci_lock,\
                         dial_mode,m_dial_mode,auto_dial,apn_mode,\
                         wan_apn,ipv4_apn,apn_name,m_apn_name,m_profile_name,profile_name,\
                         pdp_type,iccid,sim_imsi";

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

            // A cell lock pins the radio to ONE tower by EARFCN+PCI and survives
            // power cycles, so a modem carried to another location can search for
            // a tower that does not exist there and never register. It was
            // invisible here: only the band mask was reported.
            let earfcn_lock = g(&post_map, &["lte_earfcn_lock"]);
            let pci_lock = g(&post_map, &["lte_pci_lock"]);
            let locked = |s: &str| !s.trim().is_empty() && s.trim() != "0";
            if locked(&earfcn_lock) || locked(&pci_lock) {
                report.cell_lock = Some(format!("EARFCN {} / PCI {}", earfcn_lock, pci_lock));
            }

            report.dial_mode = g(&post_map, &["dial_mode", "m_dial_mode", "auto_dial"]);
            // `wan_apn` first: on MF920U-class firmware the APN lives there and
            // `apn_name`/`m_apn_name`/`profile_name` are all empty, so checking
            // only those reported "no APN configured" for a modem that had the
            // correct carrier APN set.
            report.apn = g(&post_map, &["wan_apn", "ipv4_apn", "apn_name", "m_apn_name"]);
            report.apn_profile = g(&post_map, &["m_profile_name", "profile_name"]);
            report.apn_mode = g(&post_map, &["apn_mode"]);
            report.iccid = g(&post_map, &["iccid"]);
            report.imsi = g(&post_map, &["sim_imsi"]);
        }

        let nt = report.network_type.to_uppercase();
        report.registered = !nt.is_empty() && nt != "NO_SERVICE" && !nt.starts_with("LIMITED_SERVICE") && nt != "NONE";

        // Finish the SIM verdict. An ICCID or IMSI can only be read off a SIM,
        // and a modem cannot register on a carrier without one -- either is proof
        // the card is present and readable, whatever the status field says or
        // fails to say.
        let sim_evidence = !report.iccid.is_empty() || !report.imsi.is_empty() || report.registered;
        report.sim_detected = sim_evidence || sim_status_verdict.unwrap_or(false);

        if !report.sim_detected {
            // Distinguish "the modem says there is no SIM" from "the modem says
            // nothing and nothing else suggests one", which need different advice.
            if sim_status_verdict == Some(false) {
                report.findings.push(format!(
                    "Modem reports no usable SIM in the slot (sim status: {}).",
                    if sim_st.is_empty() { "unknown" } else { &sim_st }
                ));
                report.recommendations.push("Power off modem, remove SIM card, clean contacts, re-insert firmly, and power on.".to_string());
            } else {
                report.findings.push(
                    "No SIM could be confirmed: the modem reports no SIM status, and no ICCID, IMSI or network registration was readable."
                        .to_string(),
                );
                report.recommendations.push(
                    "Check the SIM is seated. If the modem is registered on a carrier, log in (--password / ZTE_PASSWORD) -- ICCID and IMSI are auth-gated."
                        .to_string(),
                );
            }
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

        // Worth reporting whether or not the modem is currently registered: a cell
        // lock that happens to work here will strand the modem the moment it moves.
        if let Some(ref lock) = report.cell_lock {
            report.findings.push(format!(
                "Radio is locked to a single cell ({}). This survives power cycles, so the modem will fail to register anywhere that tower is not in range.",
                lock
            ));
            report.recommendations.push(
                "Run `zte-control unlock-bands` to clear the cell lock and re-enable all bands."
                    .to_string(),
            );
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
    /// Verified: the bearer returned with an address different from a *known*
    /// previous one. `previous` is not optional -- without a baseline there is
    /// nothing to have changed from, which is `UnknownBaseline` instead.
    NewIp {
        ip: String,
        previous: String,
        attempts: u32,
    },
    /// The bearer returned, but the carrier re-issued the same address every hop.
    SameIp { ip: String, attempts: u32 },
    /// The bearer returned with an address, but the address held *before* the
    /// rotation could not be read (no session, or no bearer to begin with), so
    /// "it changed" cannot be claimed.
    UnknownBaseline { ip: String, attempts: u32 },
    /// The bearer returned; its current address could not be read (no web
    /// session), so whether the address changed is unknown.
    BearerUpIpUnknown { attempts: u32 },
}

impl RotationOutcome {
    /// The post-rotation address, when one could be read.
    pub fn ip(&self) -> Option<&str> {
        match self {
            RotationOutcome::NewIp { ip, .. }
            | RotationOutcome::SameIp { ip, .. }
            | RotationOutcome::UnknownBaseline { ip, .. } => Some(ip),
            RotationOutcome::BearerUpIpUnknown { .. } => None,
        }
    }

    /// True only when the address is known to have changed -- which requires
    /// having read both the old and the new one.
    pub fn verified(&self) -> bool {
        matches!(self, RotationOutcome::NewIp { .. })
    }

    pub fn summary(&self) -> String {
        match self {
            RotationOutcome::NewIp { ip, previous, attempts } => format!(
                "new WAN IP {} (was {}) after {} band-hop(s)",
                ip, previous, attempts
            ),
            RotationOutcome::SameIp { ip, attempts } => format!(
                "bearer reset {} time(s) but the carrier kept the same WAN IP {}",
                attempts, ip
            ),
            RotationOutcome::UnknownBaseline { ip, attempts } => format!(
                "bearer reconnected after {} hop(s) with WAN IP {}, but the previous address was unreadable, so the change is unverified",
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
    /// `Some` when the radio is pinned to one tower (EARFCN + PCI). This lock
    /// persists across power cycles and can strand the modem if it is moved.
    pub cell_lock: Option<String>,
    pub rsrp: String,
    pub rssi: String,
    pub rsrq: String,
    pub sinr: String,

    pub ppp_status: String,
    pub wan_ip: String,
    pub dial_mode: String,
    pub apn: String,
    /// Human name of the active APN profile (`m_profile_name` on MF920U-class).
    pub apn_profile: String,
    /// Whether the modem picks the APN itself (`auto`) or uses a manual profile.
    pub apn_mode: String,

    pub findings: Vec<String>,
    pub recommendations: Vec<String>,
}

/// Poll `probe` until it yields a value or `timeout` of WALL-CLOCK time elapses.
///
/// The distinction matters: each probe here performs an HTTP request that can
/// itself burn `HTTP_TIMEOUT`, so a loop that counts iterations and sleeps
/// between them runs for far longer than its nominal budget. Every wait in this
/// crate goes through here so that a stated timeout is the real one.
fn poll_until<T>(timeout: Duration, mut probe: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(v) = probe() {
            return Some(v);
        }
        let now = Instant::now();
        if now >= deadline {
            return None;
        }
        thread::sleep(POLL_INTERVAL.min(deadline - now));
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

pub fn check_for_updates() -> Result<(String, String, bool)> {
    let client = Client::builder()
        .timeout(Duration::from_secs(4))
        .user_agent("zte-control-updater")
        .build()
        .map_err(|e| Error::Transport { context: "update check", source: e })?;

    let resp = client
        .get("https://api.github.com/repos/miwaniza/zte-k12-rotator/releases/latest")
        .send()
        .map_err(|e| Error::Transport { context: "update check (GitHub API)", source: e })?;

    let json: serde_json::Value = resp.json().map_err(|e| Error::Decode { context: "update check", source: e })?;
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

/// Timeout for the source-bound "is there real internet through this modem" probe.
const PROBE_TIMEOUT: Duration = Duration::from_secs(4);
/// How often the dwell loop re-checks the active modem.
const DWELL_POLL_INTERVAL: Duration = Duration::from_secs(3);

struct FleetModem {
    cfg: ModemConfig,
    client: ZTEClient,
    probe_client: Option<Client>,
}

impl FleetModem {
    fn new(cfg: ModemConfig, log: Logger) -> Self {
        let client =
            ZTEClient::new(&cfg.host, &cfg.password, Some(&cfg.bind_ip)).with_logger(Arc::clone(&log));
        let probe_client = match cfg.bind_ip.parse::<IpAddr>() {
            Ok(ip) => Client::builder()
                .local_address(ip)
                .timeout(PROBE_TIMEOUT)
                .build()
                .ok(),
            Err(_) => None,
        };
        // Say so at construction. Previously this failure was silent and
        // `is_solid` fell back to a helper that re-parsed the same bad address
        // and could only ever return false.
        if probe_client.is_none() {
            log(&format!(
                "[fleet] {}: bind_ip '{}' is not usable; probes through this modem cannot run",
                cfg.name, cfg.bind_ip
            ));
        }
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
            // A probe was requested but cannot be issued through this modem, so
            // "reachable" is unproven -- never promote it on that basis.
            (Some(_), None) => false,
            (None, _) => true,
        }
    }
}

fn run_cmd(prog: &str, args: &[&str]) -> Result<()> {
    let out = std::process::Command::new(prog)
        .args(args)
        .output()
        .map_err(|e| Error::Config(format!("`{}` failed to start: {}", prog, e)))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(Error::Config(format!(
            "`{}` exited {}: {}",
            prog,
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// Set one modem's default-route preference (lower metric = preferred).
fn set_metric(m: &ModemConfig, metric: u32) -> Result<()> {
    if cfg!(target_os = "windows") {
        let idx = m
            .iface_index
            .ok_or_else(|| Error::Config(format!("modem '{}' needs iface_index on Windows", m.name)))?;
        let arg = format!(
            "Set-NetIPInterface -InterfaceIndex {} -InterfaceMetric {}",
            idx, metric
        );
        run_cmd("powershell", &["-NoProfile", "-Command", &arg])
    } else if cfg!(target_os = "linux") {
        let dev = m
            .iface_name
            .clone()
            .ok_or_else(|| Error::Config(format!("modem '{}' needs iface_name on Linux", m.name)))?;
        let gw = m
            .gateway
            .clone()
            .ok_or_else(|| Error::Config(format!("modem '{}' needs gateway on Linux", m.name)))?;
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
                .ok_or_else(|| Error::Config(format!("modem '{}' needs gateway on macOS", m.name)))?;
            run_cmd("route", &["-n", "change", "default", &gw])
        } else {
            Ok(())
        }
    }
}

/// Make `active` the preferred uplink and demote all the others.
fn apply_active(modems: &[FleetModem], active: usize) -> Result<()> {
    set_metric(&modems[active].cfg, METRIC_ACTIVE)?;
    for (i, m) in modems.iter().enumerate() {
        if i != active {
            set_metric(&m.cfg, METRIC_STANDBY)?;
        }
    }
    Ok(())
}

fn wait_until_solid(m: &FleetModem, probe: Option<&str>, timeout_secs: u64) -> bool {
    poll_until(Duration::from_secs(timeout_secs), || {
        m.is_solid(probe).then_some(())
    })
    .is_some()
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
pub fn fleet_rotate(cfg: FleetConfig, once: bool, log: Logger) -> Result<()> {
    if cfg.modems.len() < 2 {
        return Err(Error::Config("fleet-rotate needs at least 2 modems in the config".to_string()));
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
        .ok_or_else(|| Error::Config("no modem has a solid connection to start from".to_string()))?;
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
        //
        // Wall-clock, not iteration count: `is_solid` performs a modem read plus
        // a probe request, so counting `waited += 3` per pass could stretch a
        // 90-second dwell to several minutes and silently change the fleet's
        // rotation cadence.
        let dwell_deadline = Instant::now() + Duration::from_secs(cfg.dwell_seconds);
        let mut dropped = false;
        while Instant::now() < dwell_deadline {
            let now = Instant::now();
            thread::sleep(DWELL_POLL_INTERVAL.min(dwell_deadline - now));
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
    fn rotate(&self) -> Result<RotationOutcome> {
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
        let client = client.with_logger(Arc::new(move |m: &str| {
            captured.lock().unwrap().push(m.to_string());
        }));
        client.log("hello");
        assert_eq!(sink.lock().unwrap().as_slice(), ["hello"]);
    }

    #[test]
    fn test_rotation_outcome_reporting() {
        let new_ip = RotationOutcome::NewIp {
            ip: "10.1.2.3".to_string(),
            previous: "10.9.9.9".to_string(),
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
    fn test_unknown_baseline_is_not_reported_as_verified() {
        // The case that used to be reported as a verified change: with no
        // readable pre-rotation address, `None != Some(ip)` was trivially true.
        let outcome = RotationOutcome::UnknownBaseline {
            ip: "10.1.2.3".to_string(),
            attempts: 1,
        };
        assert!(!outcome.verified());
        assert_eq!(outcome.ip(), Some("10.1.2.3"));
        assert!(outcome.summary().contains("unverified"));
    }

    #[test]
    fn test_rotation_refuses_to_run_concurrently() {
        // Nothing stopped the tray button, a scheduled script and the dashboard
        // from interleaving DISCONNECT/CONNECT against one modem.
        let client = ZTEClient::new("http://127.0.0.1:1", "pw", None);
        let held = client.rotation_guard.clone();
        let _guard = held.lock().unwrap();

        match client.rotate_verified(1) {
            Err(Error::RotationBusy) => {}
            other => panic!("expected RotationBusy, got {:?}", other),
        }
    }

    #[test]
    fn test_check_set_result_rejects_failure_bodies() {
        // The goform endpoint answers HTTP 200 with a failure in the body.
        let mut body = HashMap::new();
        body.insert("result".to_string(), serde_json::json!("failure"));
        match ZTEClient::check_set_result("BAND_SELECT", &body) {
            Err(Error::CommandRejected { goform_id, result }) => {
                assert_eq!(goform_id, "BAND_SELECT");
                assert_eq!(result, "failure");
            }
            other => panic!("expected CommandRejected, got {:?}", other),
        }
    }

    #[test]
    fn test_check_set_result_accepts_success_and_unknown_shapes() {
        let mut ok = HashMap::new();
        ok.insert("result".to_string(), serde_json::json!("success"));
        assert!(ZTEClient::check_set_result("BAND_SELECT", &ok).is_ok());

        // Some commands answer with an unrelated shape; inventing an error for
        // those would be worse than letting them through.
        let mut other = HashMap::new();
        other.insert("lte_band_lock".to_string(), serde_json::json!("0x4"));
        assert!(ZTEClient::check_set_result("BAND_SELECT", &other).is_ok());
    }

    #[test]
    fn test_poll_until_honours_wall_clock_not_iterations() {
        // A probe slower than the poll interval must not extend the budget: this
        // is exactly how a "20 second" bearer wait used to run for minutes.
        let budget = Duration::from_millis(300);
        let started = Instant::now();
        let mut probes = 0;
        let result = poll_until(budget, || {
            probes += 1;
            thread::sleep(Duration::from_millis(120));
            None::<()>
        });
        let elapsed = started.elapsed();

        assert!(result.is_none());
        assert!(probes >= 2, "should have probed more than once, got {}", probes);
        assert!(
            elapsed < budget + Duration::from_millis(400),
            "overran the budget: {:?}",
            elapsed
        );
    }

    #[test]
    fn test_poll_until_returns_first_value() {
        let mut calls = 0;
        let got = poll_until(Duration::from_secs(5), || {
            calls += 1;
            (calls == 2).then_some(calls)
        });
        assert_eq!(got, Some(2));
    }

    #[test]
    fn test_rotation_cycle_has_no_all_bands_step() {
        // "All bands" is the recovery state, not a hop: a retry landing on it
        // would not constrain the radio at all.
        assert!(!ROTATION_MASKS.iter().any(|(_, mask)| *mask == LTE_BAND_ALL));
        assert!(ROTATION_MASKS.len() >= DEFAULT_ROTATE_ATTEMPTS as usize);
    }

    /// Build a report the way `run_diagnostics` finishes it, so the SIM verdict
    /// can be exercised without a modem.
    fn sim_verdict(status_field: &str, iccid: &str, imsi: &str, registered: bool) -> bool {
        let verdict: Option<bool> = if status_field.is_empty() {
            None
        } else {
            let l = status_field.to_lowercase();
            Some(
                !l.contains("no_sim")
                    && !l.contains("error")
                    && !l.contains("none")
                    && status_field != "0",
            )
        };
        let evidence = !iccid.is_empty() || !imsi.is_empty() || registered;
        evidence || verdict.unwrap_or(false)
    }

    #[test]
    fn test_sim_present_when_firmware_reports_no_status_field() {
        // The MF920U case: neither `sim_card_status` nor `sim_state` is returned,
        // yet the SIM is plainly working. Treating the silence as absence told
        // people to clean the contacts of a SIM that had just been read.
        assert!(sim_verdict("", "8938003993073494383", "255030062405076", true));
        // Registration alone is enough -- ICCID/IMSI are auth-gated.
        assert!(sim_verdict("", "", "", true));
        // ICCID alone is enough, even with no registration.
        assert!(sim_verdict("", "8938003993073494383", "", false));
    }

    #[test]
    fn test_sim_absent_only_when_nothing_corroborates_it() {
        assert!(!sim_verdict("", "", "", false));
        assert!(!sim_verdict("NO_SIM", "", "", false));
        assert!(!sim_verdict("SIM_ERROR", "", "", false));
        assert!(!sim_verdict("0", "", "", false));
    }

    #[test]
    fn test_sim_evidence_outweighs_a_stale_status_field() {
        // A modem that says NO_SIM while handing back an IMSI and sitting on a
        // carrier network is contradicting itself; believe the evidence.
        assert!(sim_verdict("NO_SIM", "", "255030062405076", true));
    }

    #[test]
    fn test_sim_status_field_alone_can_confirm() {
        assert!(sim_verdict("READY", "", "", false));
        assert!(sim_verdict("modem_sim_ready", "", "", false));
    }

    #[test]
    fn test_error_display_and_source() {
        use std::error::Error as _;
        let e = Error::CommandRejected {
            goform_id: "BAND_SELECT".to_string(),
            result: "failure".to_string(),
        };
        assert_eq!(e.to_string(), "modem rejected BAND_SELECT: result=failure");
        assert!(e.source().is_none());
        assert_eq!(Error::RotationBusy.to_string(), "a rotation is already in progress on this modem");
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

