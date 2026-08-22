# ZTE K12 (ZX297520V3 / ZX297520V3E) Cell Tower & Band Control Suite

An intuitive control toolkit, interactive dashboard, and reverse-engineering research suite for **ZTE K12** (SmartDigital UA, firmware `BD_SMARTDIGITALUAK12V1.0.0B01`).

---

## ⚡ Quick Start: Easy Cell & Band Control

Launch the interactive terminal dashboard:
```bash
python3 tools/cell_control.py menu
```

This presents a live terminal UI to:
* View live radio signal metrics (**RSRP, RSSI, SINR, RSRQ**).
* **Lock Frequency Bands**: Band 3 (1800 MHz), Band 7 (2600 MHz), Band 8 (900 MHz), Band 20 (800 MHz).
* **Lock Cell Towers**: Input target `EARFCN` and `PCI` to lock onto a specific cell tower sector.
* **Direct WebUI Shortcuts**: One-click opening of `#developer_options` and `#network_mode` in your browser.

---

## 🛠️ CLI Quick Commands

### 1. Lock to Specific Frequency Bands
```bash
# Lock to Band 3 (1800 MHz - best urban speed)
python3 tools/cell_control.py lock-band B3

# Lock to multiple bands (e.g. B3 + B7)
python3 tools/cell_control.py lock-band B3 B7

# Reset to all Ukrainian LTE bands (B3 + B7 + B8 + B20)
python3 tools/cell_control.py lock-band ALL
```

### 2. Lock to a Specific Cell Tower (Cell Lock)
```bash
# Lock to EARFCN 1650 (Band 3) and PCI 214 with automatic RF reconnect
python3 tools/cell_control.py lock-cell --earfcn 1650 --pci 214 --reconnect

# Clear cell lock (return to automatic cell selection)
python3 tools/cell_control.py unlock-cell
```

### 3. Check Live Status & Telemetry
```bash
python3 tools/cell_control.py status
```

### 4. Open Developer Options in Web Browser
```bash
python3 tools/cell_control.py webui
```
*(Opens `http://192.168.0.1/#developer_options` directly in your default browser)*

---

## 📂 Documentation & Architecture References

* [docs/zte_k12_platform_architecture.md](docs/zte_k12_platform_architecture.md) — SoC architecture (Cortex-A53 + M0 bootloader + Baseband DSP), memory maps, SPI NAND layout.
* [docs/goform_api_reference.md](docs/goform_api_reference.md) — Reverse-engineered GoForm endpoint catalogue for cellular control and diagnostic queries.
* [docs/firmware_dump_and_mod_strategy.md](docs/firmware_dump_and_mod_strategy.md) — Firmware extraction, unpacking, and modification guide.
