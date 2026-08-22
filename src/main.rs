use clap::{Parser, Subcommand};
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::IpAddr;
use std::thread;
use std::time::Duration;
use tiny_http::{Header, Response, Server};

const EMBEDDED_UI_HTML: &str = include_str!("../web/index.html");

#[derive(Parser, Debug)]
#[command(name = "zte-control", author, version, about = "Universal macOS / Windows / Linux Controller for ZTE K12 (ZX297520)")]
pub struct Cli {
    #[arg(long, default_value = "http://192.168.0.1", help = "Router base URL")]
    pub host: String,

    #[arg(short, long, default_value = "353FALM5", help = "WebUI admin password")]
    pub password: String,

    #[arg(long, help = "Optional source IP to bind to (e.g. 192.168.0.56 when multiple routers share 192.168.0.1)")]
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

    /// Force cellular bearer disconnect & re-attach (RF reset)
    Reconnect,

    /// Launch built-in Web Control Dashboard in default browser
    Ui {
        #[arg(short, long, default_value_t = 8080, help = "Local HTTP server port")]
        port: u16,

        #[arg(long, help = "Do not automatically open browser")]
        no_open: bool,
    },
}

#[derive(Debug, Clone)]
pub struct ZTEClient {
    base_url: String,
    password: String,
    client: Client,
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

    fn sha256_upper(data: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(data.as_bytes());
        let result = hasher.finalize();
        hex::encode_upper(result)
    }

    pub fn goform_get(&self, cmd_keys: &str, multi_data: bool) -> Result<serde_json::Value, String> {
        let multi = if multi_data { "&multi_data=1" } else { "" };
        let url = format!(
            "{}/goform/goform_get_cmd_process?cmd={}{}&isTest=false&_={}",
            self.base_url,
            cmd_keys,
            multi,
            chrono_ms()
        );
        let resp = self
            .client
            .get(&url)
            .header("X-Requested-With", "XMLHttpRequest")
            .header("Referer", format!("{}/index.html", self.base_url))
            .send()
            .map_err(|e| format!("GET request failed: {}", e))?;

        resp.json().map_err(|e| format!("Invalid JSON response: {}", e))
    }

    pub fn goform_set(&self, goform_id: &str, params: &[(&str, &str)]) -> Result<serde_json::Value, String> {
        let url = format!("{}/goform/goform_set_cmd_process", self.base_url);
        let mut form = HashMap::new();
        form.insert("isTest", "false");
        form.insert("goformId", goform_id);
        for (k, v) in params {
            form.insert(k, v);
        }

        let resp = self
            .client
            .post(&url)
            .header("X-Requested-With", "XMLHttpRequest")
            .header("Referer", format!("{}/index.html", self.base_url))
            .form(&form)
            .send()
            .map_err(|e| format!("POST request failed: {}", e))?;

        resp.json().map_err(|e| format!("Invalid JSON response: {}", e))
    }

    pub fn login(&self) -> Result<(), String> {
        let ld_resp = self.goform_get("LD", false)?;
        let ld = ld_resp
            .get("LD")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let p1 = Self::sha256_upper(&self.password);
        let p_hash = Self::sha256_upper(&format!("{}{}", p1, ld));

        let res = self.goform_set("LOGIN", &[("password", &p_hash), ("save_login", "1")])?;
        if let Some(r) = res.get("result").and_then(|v| v.as_str()) {
            if r == "0" || r == "4" {
                return Ok(());
            }
        }
        Err(format!("Login rejected: {:?}", res))
    }

    pub fn get_status(&self) -> Result<serde_json::Value, String> {
        let keys = "wa_inner_version,hardware_version,modem_msn,imei,network_type,network_provider,net_select_mode,network_lte_rsrp,network_sinr,lte_rsrp,lte_rsrq,lte_snr,lte_rssi,wan_active_band,wan_active_channel,lte_pci,lte_earfcn,cell_id,lte_band_lock,sim_state,wan_ipaddr,lan_ipaddr,opms_wan_mode,loginfo,Language,web_version";
        let mut data = self.goform_get(keys, true)?;
        if data.get("loginfo").and_then(|v| v.as_str()) != Some("ok") {
            let _ = self.login();
            data = self.goform_get(keys, true)?;
        }
        Ok(data)
    }

    pub fn lock_bands(&self, bands: &[String]) -> Result<serde_json::Value, String> {
        let mut total_val: u64 = 0;
        let mut has_all = false;

        for b in bands {
            let b_up = b.trim().to_uppercase();
            match b_up.as_str() {
                "ALL" => has_all = true,
                "B3" | "3" => total_val |= 0x4,
                "B7" | "7" => total_val |= 0x40,
                "B8" | "8" => total_val |= 0x80,
                "B20" | "20" => total_val |= 0x80000,
                _ => {}
            }
        }

        let hex_mask = if has_all || total_val == 0 {
            "0x00000000000800c4".to_string()
        } else {
            format!("0x{:016x}", total_val)
        };

        println!("[*] Applying LTE Band Lock: {} (Mask: {})", bands.join(", "), hex_mask);
        self.goform_set(
            "BAND_SELECT",
            &[
                ("is_gw_band", "0"),
                ("gw_band_mask", "0"),
                ("is_lte_band", "1"),
                ("lte_band_mask", &hex_mask),
            ],
        )
    }

    pub fn lock_cell(&self, earfcn: u32, pci: u32) -> Result<serde_json::Value, String> {
        let earfcn_str = earfcn.to_string();
        let pci_str = pci.to_string();
        println!("[*] Locking cell tower: EARFCN={}, PCI={}...", earfcn, pci);
        self.goform_set(
            "LTE_LOCK_CELL_SET",
            &[
                ("lte_earfcn_lock", &earfcn_str),
                ("lte_pci_lock", &pci_str),
            ],
        )
    }

    pub fn unlock_cell(&self) -> Result<serde_json::Value, String> {
        println!("[*] Releasing cell lock (Auto cell selection)...");
        self.goform_set(
            "LTE_LOCK_CELL_SET",
            &[
                ("lte_earfcn_lock", "0"),
                ("lte_pci_lock", "0"),
            ],
        )
    }

    pub fn reconnect_rf(&self) -> Result<(), String> {
        println!("[*] Cycling cellular connection...");
        let _ = self.goform_set("DISCONNECT_NETWORK", &[]);
        thread::sleep(Duration::from_millis(1500));
        let _ = self.goform_set("CONNECT_NETWORK", &[]);
        Ok(())
    }
}

fn chrono_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn decode_bands(mask_str: &str) -> String {
    let raw = mask_str.trim_start_matches("0x");
    if let Ok(val) = u64::from_str_radix(raw, 16) {
        let mut bands = Vec::new();
        if val & 0x4 != 0 {
            bands.push("B3 (1800)");
        }
        if val & 0x40 != 0 {
            bands.push("B7 (2600)");
        }
        if val & 0x80 != 0 {
            bands.push("B8 (900)");
        }
        if val & 0x80000 != 0 {
            bands.push("B20 (800)");
        }
        if !bands.is_empty() {
            return bands.join(", ");
        }
    }
    mask_str.to_string()
}

pub fn run_ui_server(port: u16, no_open: bool) {
    let addr = format!("127.0.0.1:{}", port);
    let server = Server::http(&addr).expect("Failed to start local HTTP server");
    println!("============================================================");
    println!("  🚀 ZTE K12 Web Dashboard running at: http://{}", addr);
    println!("  Press Ctrl+C to stop.");
    println!("============================================================");

    if !no_open {
        let _ = open::that(format!("http://{}", addr));
    }

    for request in server.incoming_requests() {
        let response = Response::from_string(EMBEDDED_UI_HTML).with_header(
            Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]).unwrap(),
        );
        let _ = request.respond(response);
    }
}

fn main() {
    let cli = Cli::parse();
    let client = ZTEClient::new(&cli.host, &cli.password, cli.bind_ip.as_deref());

    match cli.command {
        None | Some(Commands::Status) => match client.get_status() {
            Ok(status) => {
                println!("============================================================");
                println!("           📡 ZTE K12 CELLULAR ROUTER STATUS");
                println!("============================================================");
                let hw = status.get("hardware_version").and_then(|v| v.as_str()).unwrap_or("K12");
                let fw = status.get("wa_inner_version").and_then(|v| v.as_str()).unwrap_or("N/A");
                let imei = status.get("imei").and_then(|v| v.as_str()).unwrap_or("N/A");
                let net_type = status.get("network_type").and_then(|v| v.as_str()).unwrap_or("N/A");
                let rsrp = status.get("lte_rsrp").or_else(|| status.get("network_lte_rsrp")).and_then(|v| v.as_str()).unwrap_or("--");
                let rssi = status.get("lte_rssi").and_then(|v| v.as_str()).unwrap_or("--");
                let sinr = status.get("lte_snr").or_else(|| status.get("network_sinr")).and_then(|v| v.as_str()).unwrap_or("--");
                let rsrq = status.get("lte_rsrq").and_then(|v| v.as_str()).unwrap_or("--");
                let band_mask = status.get("lte_band_lock").and_then(|v| v.as_str()).unwrap_or("0x0000800c4");
                let bands = decode_bands(band_mask);

                println!(" Device:        {} (Firmware: {})", hw, fw);
                println!(" IMEI:          {}", imei);
                println!(" Network State: {}", net_type);
                println!(" Signal Levels: RSRP: {} dBm | RSSI: {} dBm", rsrp, rssi);
                println!(" Quality:       SINR/SNR: {} dB | RSRQ: {} dB", sinr, rsrq);
                println!(" Allowed Bands: {} (Mask: {})", bands, band_mask);
                println!("============================================================");
            }
            Err(e) => eprintln!("[-] Failed to fetch status: {}", e),
        },

        Some(Commands::Monitor { interval }) => {
            println!("[*] Starting live cellular monitor (every {:.1}s). Press Ctrl+C to stop.", interval);
            println!("{:<8} | {:<20} | {:<8} | {:<8} | {:<8} | {:<8}", "Time", "Network State", "RSRP", "RSSI", "SINR", "RSRQ");
            println!("{}", "-".repeat(70));
            loop {
                if let Ok(st) = client.get_status() {
                    let ts = chrono_ms() / 1000 % 86400;
                    let hrs = ts / 3600;
                    let mins = (ts % 3600) / 60;
                    let secs = ts % 60;
                    let time_str = format!("{:02}:{:02}:{:02}", hrs, mins, secs);

                    let net = st.get("network_type").and_then(|v| v.as_str()).unwrap_or("N/A");
                    let rsrp = st.get("lte_rsrp").or_else(|| st.get("network_lte_rsrp")).and_then(|v| v.as_str()).unwrap_or("--");
                    let rssi = st.get("lte_rssi").and_then(|v| v.as_str()).unwrap_or("--");
                    let sinr = st.get("lte_snr").or_else(|| st.get("network_sinr")).and_then(|v| v.as_str()).unwrap_or("--");
                    let rsrq = st.get("lte_rsrq").and_then(|v| v.as_str()).unwrap_or("--");

                    println!("{:<8} | {:<20} | {:<8} | {:<8} | {:<8} | {:<8}", time_str, net, format!("{} dBm", rsrp), format!("{} dBm", rssi), format!("{} dB", sinr), format!("{} dB", rsrq));
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

        Some(Commands::Reconnect) => {
            let _ = client.reconnect_rf();
            println!("[+] Cellular reconnect triggered.");
        }

        Some(Commands::Ui { port, no_open }) => {
            run_ui_server(port, no_open);
        }
    }
}
