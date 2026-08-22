# ZTE K12 (ZX297520) Universal Control Suite

A complete, cross-platform (macOS / Windows / Linux) native Rust toolkit and modern Web Dashboard for managing **ZTE K12** 4G LTE routers (SmartDigital UA, firmware `BD_SMARTDIGITALUAK12V1.0.0B01`).

---

## 🚀 Options for Controlling the Router

### Option 1: Native Rust CLI & Universal Web Server (`zte-control`)

Compiled directly for **macOS (Apple Silicon & Intel)** and **Windows (x86_64)**:

```bash
# 1. Launch the standalone Web Dashboard in your browser:
cargo run --release -- ui
# OR directly using the binary:
./target/release/zte-control ui

# 2. Check cellular radio signal metrics (RSRP, RSSI, SINR, RSRQ):
./target/release/zte-control status

# 3. Live monitor signal in terminal (useful for antenna alignment):
./target/release/zte-control monitor

# 4. Lock specific LTE bands:
./target/release/zte-control lock-band B3
./target/release/zte-control lock-band B3 B7
./target/release/zte-control lock-band ALL

# 5. Lock specific cell tower sector (EARFCN + PCI):
./target/release/zte-control lock-cell --earfcn 1650 --pci 214 --reconnect
./target/release/zte-control unlock-cell --reconnect
```

---

### Option 2: Standalone Static Web Dashboard (`web/index.html`)

A single-file HTML5/CSS3/JavaScript web application located at [web/index.html](web/index.html).

* **Zero external dependencies**: runs directly in any browser (Chrome, Edge, Safari, Firefox).
* **Cross-platform**: works on macOS, Windows, Linux, iOS, and Android.
* **Features**:
  * Live visual meters for **RSRP, SINR, RSSI, RSRQ**.
  * 1-Click **4G Band Selection** (B3, B7, B8, B20 toggles).
  * **Cell Tower Lock Form** with EARFCN + PCI inputs.
  * Direct one-click links to the router's internal `#developer_options` and `#network_info`.

To open it in your browser:
```bash
open web/index.html
# On Windows: start web/index.html
```

---

### Option 3: Python Interactive Terminal Dashboard

```bash
python3 tools/cell_control.py menu
```

---

## 📂 Repository Structure

* [src/main.rs](src/main.rs) — Native Rust universal controller & embedded UI server.
* [web/index.html](web/index.html) — Standalone single-file modern Web Dashboard.
* [tools/cell_control.py](tools/cell_control.py) — Python interactive TUI & CLI management script.
* [docs/zte_k12_platform_architecture.md](docs/zte_k12_platform_architecture.md) — Hardware architecture, SoC specs, flash layout.
* [docs/goform_api_reference.md](docs/goform_api_reference.md) — Reverse-engineered GoForm endpoint catalogue.
* [docs/firmware_dump_and_mod_strategy.md](docs/firmware_dump_and_mod_strategy.md) — Firmware extraction and modification guide.
