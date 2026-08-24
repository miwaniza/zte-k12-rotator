use clap::{Parser, Subcommand};
use std::net::IpAddr;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use reqwest::blocking::Client;
use tiny_http::{Header, Method, Response, Server};
use zte_control::{
    check_for_updates, chrono_ms, decode_bands, fleet_rotate, get_first_non_empty, stdout_logger,
    DiagnosticReport, FleetConfig, ZTEClient, VERSION,
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

    /// No default: a shipped default password is a credential in everyone's copy
    /// of the source, and it silently authenticates against whatever modem is on
    /// the other end. Read-only commands still work without it.
    #[arg(
        short,
        long,
        env = "ZTE_PASSWORD",
        hide_env_values = true,
        help = "WebUI admin password (or set ZTE_PASSWORD)"
    )]
    pub password: Option<String>,

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

    /// Alias for `rotate`: band-hop + RF disconnect & reconnect
    Reconnect,

    /// Band-hop + bearer reset, retrying up to 3 bands until the WAN IP is verified to have changed
    Rotate,

    /// Check for application updates from GitHub
    CheckUpdate,

    /// Manage background Windows / macOS service
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },

    /// Run comprehensive offline diagnostics (SIM detection, PIN/PUK, signal, band locks, carrier registration)
    #[command(alias = "diag")]
    Diagnose {
        #[arg(long, help = "Output raw JSON format")]
        json: bool,
    },

    /// Launch the local Web Control Dashboard (loopback-only proxy to the modem WebUI)
    Ui {
        #[arg(long, default_value = "127.0.0.1", help = "Local HTTP server host")]
        host: String,

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

/// The host part of an `Origin`/`Host` authority (`host`, `host:port`, `[::1]:port`).
fn authority_host(authority: &str) -> &str {
    let a = authority.trim();
    match a.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(""),
        None => a.split(':').next().unwrap_or(""),
    }
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

/// Only pages served from this loopback server may talk to it.
///
/// This proxy carries an authenticated modem session, so a permissive answer here
/// would let any site the user happens to visit read the modem's identifiers and
/// issue SET commands. Matching is on the parsed host, not a prefix: a
/// `starts_with("http://127.0.0.1")` test would also accept
/// `http://127.0.0.1.evil.example`. `null` (sandboxed iframes, `file://`) is not
/// allowed -- an attacker can produce it at will.
fn origin_allowed(origin: &str) -> bool {
    origin
        .strip_prefix("http://")
        .map(|authority| is_loopback_host(authority_host(authority)))
        .unwrap_or(false)
}

fn header_value<'a>(request: &'a tiny_http::Request, name: &str) -> Option<&'a str> {
    request
        .headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str())
}

/// DNS-rebinding guard: an attacker-controlled name that resolves to 127.0.0.1
/// reaches this server on loopback but carries its own `Host`. Requiring a
/// loopback `Host` (or the address the operator explicitly bound to) closes that.
fn host_allowed(request: &tiny_http::Request, bind_host: &str) -> bool {
    // A wildcard bind is a deliberate "expose this" choice; there is no single
    // expected Host to check against. `run_ui_server` warns about it at startup.
    if bind_host == "0.0.0.0" || bind_host == "::" {
        return true;
    }
    match header_value(request, "host") {
        Some(h) => {
            let host = authority_host(h);
            is_loopback_host(host) || host.eq_ignore_ascii_case(bind_host)
        }
        None => false,
    }
}

/// What a request is allowed to do, decided once per request.
struct Guard {
    /// Echoed back as `Access-Control-Allow-Origin`; `None` for same-origin and
    /// non-browser callers, which need no CORS header at all.
    allow_origin: Option<String>,
    /// Set by the dashboard's XHR. A cross-origin page cannot set it without a
    /// preflight, and preflights from disallowed origins are refused -- so
    /// requiring it makes drive-by requests to the modem proxy impossible.
    xhr: bool,
}

fn forbid(request: tiny_http::Request, why: &str) {
    let body = serde_json::json!({ "error": why }).to_string();
    let response = Response::from_string(body)
        .with_status_code(403)
        .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/json; charset=utf-8"[..]).unwrap());
    let _ = request.respond(response);
}

/// A proxy failure as well-formed JSON. Building it with `format!` used to emit
/// invalid JSON whenever the transport error text contained a quote.
fn proxy_error(message: &str) -> String {
    serde_json::json!({ "error": message }).to_string()
}

fn json_response(body: String, cors: Option<&String>) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut response = Response::from_string(body)
        .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/json; charset=utf-8"[..]).unwrap())
        .with_header(Header::from_bytes(&b"Cache-Control"[..], &b"no-cache, no-store, must-revalidate"[..]).unwrap());
    if let Some(origin) = cors {
        if let Ok(h) = Header::from_bytes(&b"Access-Control-Allow-Origin"[..], origin.as_bytes()) {
            response = response.with_header(h);
            response = response.with_header(
                Header::from_bytes(&b"Vary"[..], &b"Origin"[..]).unwrap(),
            );
        }
    }
    response
}

fn handle_http_request(
    mut request: tiny_http::Request,
    client: &ZTEClient,
    http_client: &Client,
    bind_host: &str,
) {
    let url_path = request.url().to_string();

    if !host_allowed(&request, bind_host) {
        forbid(request, "unexpected Host header (possible DNS rebinding)");
        return;
    }

    let guard = match header_value(&request, "origin") {
        Some(origin) if origin_allowed(origin) => Guard {
            allow_origin: Some(origin.to_string()),
            xhr: false,
        },
        // A cross-origin caller: refuse outright rather than answering with a
        // header that happens to be restrictive.
        Some(_) => {
            forbid(request, "cross-origin requests are not allowed");
            return;
        }
        None => Guard { allow_origin: None, xhr: false },
    };
    let guard = Guard {
        xhr: header_value(&request, "x-requested-with")
            .map(|v| v.eq_ignore_ascii_case("XMLHttpRequest"))
            .unwrap_or(false),
        ..guard
    };
    let cors = guard.allow_origin.clone();

    if *request.method() == Method::Options {
        // Reached only with an allowed origin (others were refused above).
        let mut response = Response::empty(204)
            .with_header(Header::from_bytes(&b"Access-Control-Allow-Methods"[..], &b"GET, POST, OPTIONS"[..]).unwrap())
            .with_header(Header::from_bytes(&b"Access-Control-Allow-Headers"[..], &b"Content-Type, X-Requested-With"[..]).unwrap())
            .with_header(Header::from_bytes(&b"Access-Control-Max-Age"[..], &b"86400"[..]).unwrap());
        if let Some(origin) = &cors {
            if let Ok(h) = Header::from_bytes(&b"Access-Control-Allow-Origin"[..], origin.as_bytes()) {
                response = response.with_header(h);
            }
        }
        let _ = request.respond(response);
        return;
    }

    if url_path.starts_with("/manifest.json") {
        let response = Response::from_string(EMBEDDED_MANIFEST)
            .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/manifest+json; charset=utf-8"[..]).unwrap())
            .with_header(Header::from_bytes(&b"Cache-Control"[..], &b"no-cache, no-store, must-revalidate"[..]).unwrap());
        let _ = request.respond(response);
    } else if url_path.starts_with("/sw.js") {
        let response = Response::from_string(EMBEDDED_SW_JS)
            .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/javascript; charset=utf-8"[..]).unwrap())
            .with_header(Header::from_bytes(&b"Service-Worker-Allowed"[..], &b"/"[..]).unwrap())
            .with_header(Header::from_bytes(&b"Cache-Control"[..], &b"no-cache, no-store, must-revalidate"[..]).unwrap());
        let _ = request.respond(response);
    } else if url_path.starts_with("/icon.svg") {
        let response = Response::from_string(EMBEDDED_ICON_SVG)
            .with_header(Header::from_bytes(&b"Content-Type"[..], &b"image/svg+xml; charset=utf-8"[..]).unwrap());
        let _ = request.respond(response);
    } else if url_path.starts_with("/api/reconnect") || url_path.starts_with("/api/rotate") {
        // Rotating is a state change, so it must not be reachable by a GET that
        // any page can trigger with an <img> tag.
        if *request.method() != Method::Post {
            let body = serde_json::json!({
                "error": "use POST with X-Requested-With: XMLHttpRequest",
            })
            .to_string();
            let response = Response::from_string(body)
                .with_status_code(405)
                .with_header(Header::from_bytes(&b"Allow"[..], &b"POST"[..]).unwrap())
                .with_header(Header::from_bytes(&b"Content-Type"[..], &b"application/json; charset=utf-8"[..]).unwrap());
            let _ = request.respond(response);
            return;
        }
        if !guard.xhr {
            forbid(request, "missing X-Requested-With: XMLHttpRequest");
            return;
        }
        let body = match client.rotate_and_reconnect() {
            Ok(outcome) => serde_json::json!({
                "status": "success",
                "action": "rotated",
                "verified": outcome.verified(),
                "wan_ip": outcome.ip(),
                "detail": outcome.summary(),
                "outcome": outcome,
            }),
            // The old code reported `{"status":"success"}` here regardless.
            Err(e) => serde_json::json!({
                "status": "error",
                "action": "rotated",
                "verified": false,
                "wan_ip": serde_json::Value::Null,
                "detail": e,
            }),
        };
        let _ = request.respond(json_response(body.to_string(), cors.as_ref()));
    } else if url_path.starts_with("/api/update/check") {
        let body = match check_for_updates() {
            Ok((current, latest, has_update)) => serde_json::json!({
                "current": current,
                "latest": latest,
                "has_update": has_update,
            }),
            Err(e) => serde_json::json!({ "error": e }),
        };
        let _ = request.respond(json_response(body.to_string(), cors.as_ref()));
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

        let _ = request.respond(json_response(geo_data, cors.as_ref()));
    } else if url_path.starts_with("/goform/") {
        // The proxy replays this process's authenticated modem session, so it is
        // gated the same way as the rotate endpoints.
        if !guard.xhr {
            forbid(request, "missing X-Requested-With: XMLHttpRequest");
            return;
        }
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
                Ok(r) => {
                    // Capture the session cookie from a login forwarded through the
                    // proxy, so subsequent forwarded SETs carry it (the cookie store
                    // is disabled; see ZTEClient::session_cookie).
                    client.capture_session_cookie(&r);
                    r.text().unwrap_or_else(|e| proxy_error(&e.to_string()))
                }
                Err(e) => proxy_error(&e.to_string()),
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
            match req.send().and_then(|r| r.text()) {
                Ok(body) => body,
                Err(e) => proxy_error(&e.to_string()),
            }
        };

        let _ = request.respond(json_response(result_body, cors.as_ref()));
    } else {
        let response = Response::from_string(EMBEDDED_UI_HTML)
            .with_header(Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]).unwrap())
            .with_header(Header::from_bytes(&b"Cache-Control"[..], &b"no-cache, no-store, must-revalidate"[..]).unwrap())
            .with_header(Header::from_bytes(&b"Pragma"[..], &b"no-cache"[..]).unwrap());
        let _ = request.respond(response);
    }
}

pub fn run_ui_server(client: Arc<ZTEClient>, host: &str, port: u16, no_open: bool) -> Result<(), String> {
    let server_addr = format!("{}:{}", host, port);
    let server = Server::http(&server_addr).map_err(|e| {
        format!(
            "cannot listen on {}: {}\n    (--host takes an IP address, e.g. 127.0.0.1; check the port is free)",
            server_addr, e
        )
    })?;
    let server = Arc::new(server);

    println!("============================================================");
    println!("  🚀 ZTE K12 Master Web Controller & PWA v{} Started", VERSION);
    println!("  👉 Dashboard: http://{}:{}", host, port);
    println!("  📡 Router:    {}", client.base_url);
    println!("============================================================");

    if !is_loopback_host(host) {
        eprintln!("  ⚠️  Listening on {} -- NOT loopback. This server proxies an", host);
        eprintln!("      authenticated modem session; anyone who can reach this port can");
        eprintln!("      control the modem. Use --host 127.0.0.1 unless you mean it.");
    }

    if !no_open {
        let url = format!("http://{}:{}", host, port);
        let _ = open::that(url);
    }

    let http_client = Client::builder()
        .timeout(Duration::from_secs(5))
        .user_agent("zte-control-server")
        .build()
        .unwrap_or_else(|_| Client::new());

    let mut handles = Vec::new();
    let num_workers = 4;

    for _ in 0..num_workers {
        let server = Arc::clone(&server);
        let client = Arc::clone(&client);
        let http_client = http_client.clone();
        let bind_host = host.to_string();
        let handle = thread::spawn(move || {
            for request in server.incoming_requests() {
                handle_http_request(request, &client, &http_client, &bind_host);
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        let _ = handle.join();
    }
    Ok(())
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
                    .args(["/Create", "/TN", "ZTEK12RotatorService", "/TR", &format!("\"{}\" ui --no-open", exe_str), "/SC", "ONLOGON", "/RL", "HIGHEST", "/F"])
                    .status();
                if status.map(|s| s.success()).unwrap_or(false) {
                    println!("[+] Successfully installed background service!");
                    let _ = Command::new("schtasks").args(["/Run", "/TN", "ZTEK12RotatorService"]).status();
                    println!("[+] Service started at http://127.0.0.1:8080");
                } else {
                    eprintln!("[-] Failed to install service. Try running as Administrator.");
                }
            }
            ServiceAction::Uninstall => {
                println!("[*] Removing Windows Scheduled Background Task: ZTEK12RotatorService");
                let _ = Command::new("schtasks").args(["/End", "/TN", "ZTEK12RotatorService"]).status();
                let _ = Command::new("schtasks").args(["/Delete", "/TN", "ZTEK12RotatorService", "/F"]).status();
                println!("[+] Service uninstalled.");
            }
            ServiceAction::Start => {
                let _ = Command::new("schtasks").args(["/Run", "/TN", "ZTEK12RotatorService"]).status();
                println!("[+] Background task start requested.");
            }
            ServiceAction::Stop => {
                let _ = Command::new("schtasks").args(["/End", "/TN", "ZTEK12RotatorService"]).status();
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

                if fs::write(&plist_path, plist_content).is_ok() {
                    let _ = Command::new("launchctl").args(["unload", "-w", &plist_path]).status();
                    let status = Command::new("launchctl").args(["load", "-w", &plist_path]).status();
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
                let _ = Command::new("launchctl").args(["unload", "-w", &plist_path]).status();
                let _ = fs::remove_file(&plist_path);
                println!("[+] Service uninstalled.");
            }
            ServiceAction::Start => {
                let _ = Command::new("launchctl").args(["start", "com.zte.rotator"]).status();
                println!("[+] LaunchAgent started.");
            }
            ServiceAction::Stop => {
                let _ = Command::new("launchctl").args(["stop", "com.zte.rotator"]).status();
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


/// Commands that issue SET requests are useless without a password; say so once,
/// clearly, instead of letting every call fail with an auth error.
fn require_password(password: &str) {
    if password.is_empty() {
        eprintln!("[-] This command needs the WebUI admin password.");
        eprintln!("    Pass --password <pw> or set ZTE_PASSWORD in the environment.");
        std::process::exit(2);
    }
}

fn main() {
    let cli = Cli::parse();
    let password = cli.password.clone().unwrap_or_default();
    let client = Arc::new(ZTEClient::new(&cli.host, &password, cli.bind_ip.as_deref()));

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
                    let total_secs = (chrono_ms() / 1000) % 86400;
                    let hrs = total_secs / 3600;
                    let mins = (total_secs % 3600) / 60;
                    let secs = total_secs % 60;
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

        Some(Commands::LockBand { bands }) => {
            require_password(&password);
            match client.lock_bands(&bands) {
                Ok(res) => println!("[+] Band lock result: {}", res),
                Err(e) => eprintln!("[-] Error locking band: {}", e),
            }
        }

        Some(Commands::LockCell { earfcn, pci, reconnect }) => {
            require_password(&password);
            match client.lock_cell(earfcn, pci) {
                Ok(res) => {
                    println!("[+] Cell lock result: {}", res);
                    if reconnect {
                        report_rotation(client.rotate_and_reconnect());
                    }
                }
                Err(e) => eprintln!("[-] Error locking cell: {}", e),
            }
        }

        Some(Commands::UnlockCell { reconnect }) => {
            require_password(&password);
            match client.unlock_cell() {
                Ok(res) => {
                    println!("[+] Unlock result: {}", res);
                    if reconnect {
                        report_rotation(client.rotate_and_reconnect());
                    }
                }
                Err(e) => eprintln!("[-] Error unlocking cell: {}", e),
            }
        }

        Some(Commands::UnlockBands) => {
            require_password(&password);
            match client.unlock_bands() {
                Ok(res) => println!("[+] All bands re-enabled (2G/3G + LTE), locks cleared: {}", res),
                Err(e) => eprintln!("[-] Error re-enabling bands: {}", e),
            }
        }

        Some(Commands::Reconnect) | Some(Commands::Rotate) => {
            require_password(&password);
            if !report_rotation(client.rotate_and_reconnect()) {
                std::process::exit(1);
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

        Some(Commands::Ui { host, port, no_open }) => {
            if let Err(e) = run_ui_server(client, &host, port, no_open) {
                eprintln!("[-] {}", e);
                std::process::exit(1);
            }
        }

        Some(Commands::Diagnose { json }) => {
            let report = client.run_diagnostics();
            print_diagnostics(&report, json);
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
                    if let Err(e) = fleet_rotate(fc, once, stdout_logger()) {
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

/// Report a rotation without overstating it. Returns true when the public address
/// is known to have changed.
fn report_rotation(result: Result<zte_control::RotationOutcome, String>) -> bool {
    match result {
        Ok(outcome) if outcome.verified() => {
            println!("[+] Cellular session rotated: {}", outcome.summary());
            true
        }
        Ok(outcome) => {
            println!("[!] Rotation incomplete: {}", outcome.summary());
            false
        }
        Err(e) => {
            eprintln!("[-] Error during rotation: {}", e);
            false
        }
    }
}

fn print_diagnostics(rep: &DiagnosticReport, json_output: bool) {
    if json_output {
        if let Ok(j) = serde_json::to_string_pretty(rep) {
            println!("{}", j);
            return;
        }
    }

    println!("============================================================");
    println!("        🔍 ZTE MODEM OFFLINE DIAGNOSTICS & HEALTH CHECK");
    println!("============================================================");
    println!(" Target Host:      {}", rep.host);
    println!(" Reachable:        {}", if rep.reachable { "✅ Yes" } else { "❌ No (Check USB / RNDIS network adapter)" });
    println!(" Auth Status:      {}", if rep.authenticated { "✅ Authenticated" } else if rep.login_lock_seconds > 0 { "🔒 Locked Out" } else { "⚠️  Unauthenticated (Read-Only)" });
    if rep.login_lock_seconds > 0 {
        println!(" Lockout Left:     ~{} seconds", rep.login_lock_seconds);
    }
    if !rep.hardware_version.is_empty() || !rep.firmware_version.is_empty() {
        println!(" Hardware / FW:    {} | {}", rep.hardware_version, rep.firmware_version);
    }
    if !rep.imei.is_empty() {
        println!(" IMEI / SN:        {} | {}", rep.imei, rep.modem_sn);
    }
    if !rep.battery.is_empty() {
        println!(" Battery Level:    {}%", rep.battery);
    }
    if !rep.wifi_devices.is_empty() {
        println!(" WiFi Clients:     {}", rep.wifi_devices);
    }
    println!("------------------------------------------------------------");
    println!(" 💳 SIM CARD STATUS");
    println!(" SIM Detected:     {}", if rep.sim_detected { "✅ Yes (SIM Detected)" } else { "❌ NOT DETECTED (NO_SIM / Check Slot)" });
    println!(" SIM Raw State:    {}", if rep.sim_state.is_empty() { "Unknown" } else { &rep.sim_state });
    println!(" PIN Lock:         {}", if rep.pin_status.is_empty() { "None" } else { &rep.pin_status });
    println!(" PUK Lock:         {}", if rep.puk_status.is_empty() { "None" } else { &rep.puk_status });
    if !rep.iccid.is_empty() {
        println!(" ICCID:            {}", rep.iccid);
    }
    if !rep.imsi.is_empty() {
        println!(" IMSI:             {}", rep.imsi);
    }
    println!("------------------------------------------------------------");
    println!(" 📡 CELLULAR RADIO & NETWORK");
    println!(" Network Status:   {} ({})", if rep.registered { "✅ Registered" } else { "❌ NO SERVICE / Searching" }, if rep.network_type.is_empty() { "N/A" } else { &rep.network_type });
    println!(" Carrier / PLMN:   {}", if rep.provider.is_empty() { "N/A" } else { &rep.provider });
    println!(" Roaming:          {}", if rep.roaming { "⚠️  Roaming" } else { "Home Network" });
    println!(" Active Band:      {} (Channel / EARFCN: {})", if rep.band.is_empty() { "N/A" } else { &rep.band }, if rep.channel.is_empty() { "--" } else { &rep.channel });
    println!(" Serving Cell:     PCI: {} | Cell ID: {}", if rep.pci.is_empty() { "--" } else { &rep.pci }, if rep.cell_id.is_empty() { "--" } else { &rep.cell_id });
    println!(" Allowed Bands:    {}", rep.band_lock);
    println!(" Radio Signal:     RSRP: {} dBm | RSSI: {} dBm", if rep.rsrp.is_empty() { "--" } else { &rep.rsrp }, if rep.rssi.is_empty() { "--" } else { &rep.rssi });
    println!(" Signal Quality:   SINR: {} dB | RSRQ: {} dB", if rep.sinr.is_empty() { "--" } else { &rep.sinr }, if rep.rsrq.is_empty() { "--" } else { &rep.rsrq });
    println!("------------------------------------------------------------");
    println!(" 🌐 DATA BEARER & IP");
    println!(" Data Bearer (PPP):{}", if rep.ppp_status == "ppp_connected" { "✅ Connected" } else { "❌ Disconnected" });
    println!(" Assigned WAN IP:  {}", if rep.wan_ip.is_empty() { "None" } else { &rep.wan_ip });
    println!(" APN Profile:      {}", if rep.apn.is_empty() { "None / Default" } else { &rep.apn });
    println!(" Dial Mode:        {}", if rep.dial_mode.is_empty() { "N/A" } else { &rep.dial_mode });
    println!("============================================================");

    if !rep.findings.is_empty() {
        println!(" ⚠️  DIAGNOSTIC FINDINGS:");
        for f in &rep.findings {
            println!("  [!] {}", f);
        }
        println!("------------------------------------------------------------");
    }

    if !rep.recommendations.is_empty() {
        println!(" 💡 ACTIONABLE RECOMMENDATIONS:");
        for (i, r) in rep.recommendations.iter().enumerate() {
            println!("  {}. {}", i + 1, r);
        }
        println!("============================================================");
    } else if rep.ppp_status == "ppp_connected" {
        println!("  🎉 All systems operational. Modem is healthy and connected.");
        println!("============================================================");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_authority_host_splits_port_and_ipv6() {
        assert_eq!(authority_host("127.0.0.1"), "127.0.0.1");
        assert_eq!(authority_host("127.0.0.1:8080"), "127.0.0.1");
        assert_eq!(authority_host("localhost:8080"), "localhost");
        assert_eq!(authority_host("[::1]:8080"), "::1");
        assert_eq!(authority_host("[::1]"), "::1");
    }

    #[test]
    fn test_is_loopback_host() {
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("127.0.0.53"));
        assert!(is_loopback_host("::1"));
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("LOCALHOST"));

        assert!(!is_loopback_host("192.168.8.1"));
        assert!(!is_loopback_host("evil.example"));
        assert!(!is_loopback_host(""));
    }

    #[test]
    fn test_origin_allowed_accepts_only_loopback_pages() {
        assert!(origin_allowed("http://127.0.0.1:8080"));
        assert!(origin_allowed("http://localhost:8080"));
        assert!(origin_allowed("http://[::1]:8080"));
    }

    #[test]
    fn test_origin_allowed_rejects_hostile_origins() {
        // The bug this replaces: every one of these previously fell through to
        // `Access-Control-Allow-Origin: *`, which let any page read the modem
        // proxy's responses.
        assert!(!origin_allowed("https://evil.example"));
        assert!(!origin_allowed("http://evil.example"));
        // Prefix-matching lookalikes.
        assert!(!origin_allowed("http://127.0.0.1.evil.example"));
        assert!(!origin_allowed("http://localhost.evil.example"));
        // Sandboxed iframes and file:// pages, which an attacker can produce.
        assert!(!origin_allowed("null"));
        // The Tauri shell this once allowed no longer exists.
        assert!(!origin_allowed("tauri://localhost"));
    }
}
