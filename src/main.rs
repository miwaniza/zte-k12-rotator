use clap::{Parser, Subcommand};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use reqwest::blocking::Client;
use tiny_http::{Header, Method, Response, Server};
use zte_control::{
    check_for_updates, chrono_ms, decode_bands, fleet_rotate, get_first_non_empty, FleetConfig,
    ZTEClient, VERSION,
};

const EMBEDDED_UI_HTML: &str = include_str!("../web/index.html");
const EMBEDDED_MANIFEST: &str = include_str!("../web/manifest.json");
const EMBEDDED_SW_JS: &str = include_str!("../web/sw.js");
const EMBEDDED_ICON_SVG: &str = include_str!("../web/icon.svg");



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

    /// Re-enable ALL bands (2G/3G + every LTE band) and clear locks -- recovers a
    /// modem stuck in NO_SERVICE after a narrow band lock
    UnlockBands,

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

    /// Make-before-break IP rotation across multiple modems (see docs/multi_modem_rotation.md)
    FleetRotate {
        #[arg(long, help = "Path to fleet JSON config")]
        config: String,

        #[arg(long, help = "Run a single rotate+swap cycle and exit (default: loop)")]
        once: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum ServiceAction {
    Install,
    Uninstall,
    Start,
    Stop,
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

        Some(Commands::UnlockBands) => match client.unlock_bands() {
            Ok(res) => println!("[+] All bands re-enabled (2G/3G + LTE), locks cleared: {}", res),
            Err(e) => eprintln!("[-] Error re-enabling bands: {}", e),
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

        Some(Commands::FleetRotate { config, once }) => {
            let parsed = std::fs::read_to_string(&config)
                .map_err(|e| format!("cannot read config {}: {}", config, e))
                .and_then(|s| {
                    serde_json::from_str::<FleetConfig>(&s)
                        .map_err(|e| format!("invalid fleet config JSON: {}", e))
                });
            match parsed {
                Ok(fc) => {
                    if let Err(e) = fleet_rotate(fc, once) {
                        eprintln!("[fleet] error: {}", e);
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    eprintln!("{}", e);
                    std::process::exit(1);
                }
            }
        }
    }
}
