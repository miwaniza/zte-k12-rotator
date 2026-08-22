# ZTE K12 (ZX297520V3 / ZX297520V3E) Research & Customization Suite

A comprehensive toolkit, exploitation suite, and architectural guide for researching, unlocking, and customizing **ZTE K12** (and related ZTE / Sanechips ZX297520V3 / ZX297520V3E cellular routers).

---

## 🚀 Key Capabilities

* **Cell Locking (`AT+ZLTELC`)**: Lock to specific LTE Downlink EARFCN and Physical Cell ID (PCI) via GoAhead `goform` IPC without modifying baseband binaries.
* **Band Locking & Bearer Preference**: Enforce LTE-only modes and custom band profiles.
* **Fast Carrier / RF Reset**: Force bearer drop and re-attach for rapid cell switching.
* **Privilege Escalation & Root Shell**: Exploits unauthenticated directory traversal and post-auth `system()` command injection (`REMOVE_WHITE_SITE`, `TZ_CMD_SECURE_LOGIN`) to spawn root `telnetd` or activate ADB.
* **Firmware Extraction & Unpacking**: Live MTD dump guides and standalone SPI NAND OOB stripping / partition extraction tool.
* **USB BootROM RAM Bootloader**: Documentation for using `zx297520v3-loader` to boot custom U-Boot into RAM via USB download protocol (`19d2:0256`) with secure boot bypass.

---

## 📂 Repository Structure

* [docs/zte_k12_platform_architecture.md](docs/zte_k12_platform_architecture.md) — Detailed hardware, boot MCU, Cortex-A53 AP, partition layout, and modem IPC architecture.
* [docs/goform_api_reference.md](docs/goform_api_reference.md) — Complete reverse-engineered GoForm endpoint catalog for cellular metrics, cell lock, band control, and exploits.
* [docs/firmware_dump_and_mod_strategy.md](docs/firmware_dump_and_mod_strategy.md) — Three-tier methodology for firmware extraction (Live shell, USB BootROM, Hardware SPI NAND) and rootfs modding.
* [tools/zte_client.py](tools/zte_client.py) — Interactive Python CLI client for device probing, live RF signal reading, cell locking, bearer resetting, and exploit execution.
* [tools/dump_parser.py](tools/dump_parser.py) — Zero-dependency SPI NAND dump parser with OOB stripping and partition splitting.

---

## 🛠️ Usage Examples

### 1. Device Discovery & Live Signal Metrics
```bash
python3 tools/zte_client.py --host 192.168.0.1 probe
```

### 2. LTE Cell Lock (EARFCN + PCI)
```bash
# Lock to EARFCN 1650 (Band 3), PCI 214 and automatically reconnect RF
python3 tools/zte_client.py --host 192.168.0.1 lock-cell --earfcn 1650 --pci 214 --reconnect

# Return to automatic cell selection
python3 tools/zte_client.py --host 192.168.0.1 unlock-cell --reconnect
```

### 3. Spawn Root Telnet Shell (Exploit)
```bash
python3 tools/zte_client.py --host 192.168.0.1 enable-telnet
telnet 192.168.0.1
```

### 4. Extract Plaintext Admin Password (Pre-Auth)
```bash
python3 tools/zte_client.py --host 192.168.0.1 get-password
```

### 5. Parse Raw SPI NAND Dump
```bash
python3 tools/dump_parser.py raw_flash_dump.bin -o extracts/
```
