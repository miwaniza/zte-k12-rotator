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

## 🚀 Key Features

* **✈️ Airplane Reconnect (ЯК НА IPHONE)**: Native baseband radio reset cycling Kyivstar cellular bearer to acquire a brand-new WAN & Public IP in ~3s.
* **🛡️ Guaranteed Distinct IP Loop**: Automatically verifies `New IP != Prior IP` upon rotation, cycling frequency bands (Band 3 ⇄ Band 8 ⇄ Band 7 ⇄ Band 20) until a guaranteed different IP is assigned.
* **🌐 Current vs Prior Comparison Table**: Instant property-by-property comparison (Public IP, Location, ISP, Tower, Signal RSRP, WAN IP) with smooth entry animations.
* **🗼 Master Tower Catalog**: Live scanner for Band 3 (1800), Band 7 (2600), Band 8 (900), Band 20 (800) with 4-bar signal meters, frequency tabs, and safe fallback watchdog.
* **🔌 REST API / Webhooks**:
  * Trigger Reconnect / New IP: `curl http://127.0.0.1:8080/api/reconnect`
  * Trigger Band-Hop Rotate: `curl http://127.0.0.1:8080/api/rotate`
  * Query Geo-IP: `curl http://127.0.0.1:8080/api/geo`
