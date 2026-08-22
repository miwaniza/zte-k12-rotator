use clap::{Parser, Subcommand};
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Read;
use std::net::IpAddr;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tiny_http::{Header, Method, Response, Server};

const EMBEDDED_UI_HTML: &str = include_str!("../web/index.html");

#[derive(Parser, Debug)]
#[command(name = "zte-control", author, version, about = "Universal macOS / Windows / Linux Controller for ZTE K12 (ZX297520)")]
pub struct Cli {
    #[arg(long, default_value = "http://192.168.0.1", help = "Router base URL")]
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

    /// Force cellular bearer disconnect & re-attach (RF reset / Airplane mode)
    Reconnect,

    /// Rotate to next LTE band and obtain a new IP address
    Rotate,

    /// Launch built-in Web Control Dashboard with automated CORS proxy
    Ui {
        #[arg(short, long, default_value_t = 8080, help = "Local HTTP server port")]
        port: u16,

        #[arg(long, help = "Do not automatically open browser")]
        no_open: bool,
    },
}

#[derive(Debug, Clone)]
pub struct ZTEClient {
    pub base_url: String,
    pub password: String,
    pub client: Client,
}

impl ZTEClient {
    pub fn new(host: &str, password: &str, bind_ip: Option<&str>) -> Self {
        let base_url = host.trim_end_matches('/').to_string();
        let mut builder = Client::builder()
            .cookie_store(true)
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
        }
    }

    pub fn sha256_hex_upper(input: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(input.as_bytes());
        let result = hasher.finalize();
        hex::encode(result).to_uppercase()
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

        let resp = self
            .client
            .get(&url)
            .header("X-Requested-With", "XMLHttpRequest")
            .header("Referer", format!("{}/index.html", self.base_url))
            .send()
            .map_err(|e| format!("HTTP GET error: {}", e))?;

        let json_map: HashMap<String, serde_json::Value> = resp
            .json()
            .map_err(|e| format!("JSON decode error: {}", e))?;
        Ok(json_map)
    }

    pub fn get_ad_token(&self) -> Result<String, String> {
        let rd_map = self.get_cmd("RD", false)?;
        let rd = rd_map
            .get("RD")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let fw_hash = Self::sha256_hex_upper("BD_SMARTDIGITALUAK12V1.0.0B01");
        let ad = Self::sha256_hex_upper(&format!("{}{}", fw_hash, rd));
        Ok(ad)
    }

    pub fn login(&self) -> Result<bool, String> {
        let ld_map = self.get_cmd("LD", false)?;
        let ld = ld_map
            .get("LD")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let p1 = Self::sha256_hex_upper(&self.password);
        let password_hash = Self::sha256_hex_upper(&format!("{}{}", p1, ld));

        let url = format!("{}/goform/goform_set_cmd_process", self.base_url);
        let mut params = HashMap::new();
        params.insert("isTest".to_string(), "false".to_string());
        params.insert("goformId".to_string(), "LOGIN".to_string());
        params.insert("password".to_string(), password_hash);
        params.insert("save_login".to_string(), "1".to_string());

        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/x-www-form-urlencoded; charset=UTF-8")
            .header("X-Requested-With", "XMLHttpRequest")
            .header("Referer", format!("{}/index.html", self.base_url))
            .form(&params)
            .send()
            .map_err(|e| format!("Login POST error: {}", e))?;

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
        let status = self.get_cmd("loginfo", false)?;
        if let Some(s) = status.get("loginfo").and_then(|v| v.as_str()) {
            if s == "ok" {
                return Ok(());
            }
        }
        if !self.login()? {
            return Err("Failed to authenticate to ZTE K12 WebUI".to_string());
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
        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/x-www-form-urlencoded; charset=UTF-8")
            .header("X-Requested-With", "XMLHttpRequest")
            .header("Referer", format!("{}/index.html", self.base_url))
            .form(&params)
            .send()
            .map_err(|e| format!("POST command error: {}", e))?;

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

    pub fn reconnect_rf(&self) -> Result<(), String> {
        let _ = self.unlock_cell();
        let mut p1 = HashMap::new();
        p1.insert("notCallback".to_string(), "true".to_string());
        let _ = self.post_cmd("DISCONNECT_NETWORK", p1, true);
        thread::sleep(Duration::from_millis(1500));
        let mut p2 = HashMap::new();
        p2.insert("notCallback".to_string(), "true".to_string());
        let _ = self.post_cmd("CONNECT_NETWORK", p2, true);
        Ok(())
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

pub fn run_ui_server(client: Arc<ZTEClient>, port: u16, no_open: bool) {
    let server_addr = format!("0.0.0.0:{}", port);
    let server = Server::http(&server_addr).expect("Failed to start HTTP server");

    println!("============================================================");
    println!("  🚀 ZTE K12 Master Web Controller & IP Rotator Started");
    println!("  👉 URL: http://127.0.0.1:{}", port);
    println!("  📡 Router: {}", client.base_url);
    println!("============================================================");

    if !no_open {
        let url = format!("http://127.0.0.1:{}", port);
        let _ = open::that(url);
    }

    let http_client = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_else(|_| Client::new());

    for mut request in server.incoming_requests() {
        let url_path = request.url().to_string();

        if url_path.starts_with("/api/reconnect") || url_path.starts_with("/api/rotate") {
            let _ = client.reconnect_rf();
            let response = Response::from_string("{\"status\":\"success\",\"action\":\"reconnected\"}")
                .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/json; charset=utf-8"[..]).unwrap())
                .with_header(Header::from_bytes(&b"Access-Control-Allow-Origin"[..], &b"*"[..]).unwrap());
            let _ = request.respond(response);
        } else if url_path.starts_with("/api/geo") {
            // Geolocation proxy
            let geo_data = http_client
                .get("https://ipwho.is/")
                .send()
                .and_then(|r| r.text())
                .unwrap_or_else(|_| "{\"success\":false,\"error\":\"geo_fetch_failed\"}".to_string());

            let response = Response::from_string(geo_data)
                .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/json; charset=utf-8"[..]).unwrap())
                .with_header(Header::from_bytes(&b"Access-Control-Allow-Origin"[..], &b"*"[..]).unwrap());
            let _ = request.respond(response);
        } else if url_path.starts_with("/goform/") {
            let target_url = format!("{}{}", client.base_url, url_path);

            let result_body = if *request.method() == Method::Post {
                let mut body_bytes = Vec::new();
                let _ = request.as_reader().read_to_end(&mut body_bytes);
                client
                    .client
                    .post(&target_url)
                    .header("Content-Type", "application/x-www-form-urlencoded; charset=UTF-8")
                    .header("X-Requested-With", "XMLHttpRequest")
                    .header("Referer", format!("{}/index.html", client.base_url))
                    .body(body_bytes)
                    .send()
                    .and_then(|r| r.text())
                    .unwrap_or_else(|e| format!("{{\"error\":\"{}\"}}", e))
            } else {
                client
                    .client
                    .get(&target_url)
                    .header("X-Requested-With", "XMLHttpRequest")
                    .header("Referer", format!("{}/index.html", client.base_url))
                    .send()
                    .and_then(|r| r.text())
                    .unwrap_or_else(|e| format!("{{\"error\":\"{}\"}}", e))
            };

            let response = Response::from_string(result_body)
                .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/json; charset=utf-8"[..]).unwrap())
                .with_header(Header::from_bytes(&b"Access-Control-Allow-Origin"[..], &b"*"[..]).unwrap());
            let _ = request.respond(response);
        } else {
            let response = Response::from_string(EMBEDDED_UI_HTML)
                .with_header(Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]).unwrap());
            let _ = request.respond(response);
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
                println!("           📡 ZTE K12 CELLULAR ROUTER STATUS");
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
                    let _ = client.reconnect_rf();
                }
            }
            Err(e) => eprintln!("[-] Error locking cell: {}", e),
        },

        Some(Commands::UnlockCell { reconnect }) => match client.unlock_cell() {
            Ok(res) => {
                println!("[+] Unlock result: {}", res);
                if reconnect {
                    let _ = client.reconnect_rf();
                }
            }
            Err(e) => eprintln!("[-] Error unlocking cell: {}", e),
        },

        Some(Commands::Reconnect) | Some(Commands::Rotate) => {
            let _ = client.reconnect_rf();
            println!("[+] Cellular Airplane reconnect triggered. New IP requested.");
        }

        Some(Commands::Ui { port, no_open }) => {
            run_ui_server(client, port, no_open);
        }
    }
}
