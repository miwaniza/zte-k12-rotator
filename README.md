# 📡 ZTE K12 Mobile Controller & IP Rotator

Universal LTE Band Lock, Cell Locking, Guaranteed IP Rotator, and Web Control Dashboard for **ZTE K12** (ZX297520 / Kyivstar 4G LTE).

---

## ⚡ 1-Click Windows Installation (PowerShell)

Open **PowerShell** and run:

```powershell
irm https://raw.githubusercontent.com/miwaniza/zte-k12-rotator/main/install.ps1 | iex
```

### What this does:
1. Automatically downloads and unpacks `zte-control.exe`, 1-click batch scripts, and web UI to `%LOCALAPPDATA%\zte-k12-rotator`.
2. Creates a **Desktop Shortcut** `ZTE K12 Controller` and **Start Menu** shortcut.
3. Adds `zte-control` to your user `PATH`.
4. Automatically launches the Web Dashboard at `http://127.0.0.1:8080`.

---

## ⚡ 1-Click macOS / Linux Installation (Terminal)

Open **Terminal** and run:

```bash
curl -fsSL https://raw.githubusercontent.com/miwaniza/zte-k12-rotator/main/install.sh | bash
```

---

## 🔑 Credentials

There is no built-in password. Pass `--password <pw>` or set `ZTE_PASSWORD`:

```bash
export ZTE_PASSWORD='your-webui-password'   # PowerShell: $env:ZTE_PASSWORD = '...'
zte-control rotate
```

Read-only commands (`status`, `monitor`, `diagnose`) work unauthenticated; the
auth-gated fields (WAN IP, active band, cell identifiers) will be blank.

## 🚀 Key Features

* **✈️ Band-hop bearer reset**: `DISCONNECT_NETWORK` / `CONNECT_NETWORK` around an LTE band change, to get a fresh carrier-assigned address. One hop takes roughly 15-45s: the modem has to re-register on the new band before it can dial.
* **🛡️ Verified rotation**: the WAN IP is read *before* the rotation and compared afterwards. If the carrier re-issues the same address, the next band in the cycle (Band 8 ⇄ 3 ⇄ 7 ⇄ 20 ⇄ All) is tried, up to 3 hops. The result says which of three things happened — address changed, address unchanged, or address unreadable — instead of reporting every completed bearer reset as a success.
* **🩺 Self-healing**: 2G/3G stay enabled behind every LTE band selection, and a hop that finds no coverage restores all bands and re-dials, so a rotation cannot strand the modem in `NO_SERVICE`.
* **🗼 Tower catalog**: records the serving cell as the modem camps on it, and can sweep Band 3 / 7 / 8 / 20 in turn to discover one per band. Only cells the modem actually reported are listed.
* **🖥️ Two front-ends**: a local web dashboard (`zte-control ui`) and a native desktop app (`zte-egui`, no webview).
* **🔗 Fleet mode**: make-before-break rotation across two or more modems for a rotating IP with no connectivity gap — see [docs/multi_modem_rotation.md](docs/multi_modem_rotation.md).

## 🔌 REST API

`zte-control ui` listens on `127.0.0.1:8080` only. It proxies an **authenticated**
modem session, so mutating endpoints require `POST` plus
`X-Requested-With: XMLHttpRequest`, cross-origin requests are refused, and the
`Host` header must be loopback. That combination is what stops a web page you
happen to be visiting from driving your modem.

```bash
# Rotate (blocks until the bearer is back; up to ~2 min for 3 hops)
curl -X POST -H "X-Requested-With: XMLHttpRequest" http://127.0.0.1:8080/api/rotate

# {"status":"success","verified":true,"wan_ip":"10.x.x.x",
#  "detail":"new WAN IP 10.x.x.x (was 10.y.y.y) after 1 band-hop(s)", ...}
#
# `verified` is the field to branch on. `status:"success"` only means the request
# completed; `verified:false` means the address did not change or could not be read.

curl http://127.0.0.1:8080/api/geo            # geo-IP of the current public address
curl http://127.0.0.1:8080/api/update/check   # latest release vs. running version
```
