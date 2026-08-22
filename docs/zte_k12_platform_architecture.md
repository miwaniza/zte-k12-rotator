# ZTE K12 (ZX297520V3 / ZX297520V3E) Platform Architecture & Research Guide

## 1. Executive Summary

The **ZTE K12** (and related carrier-branded variants such as Vodafone K12) is built upon the **Sanechips (ZXIC / ZTE Microelectronics) ZX297520V3 / ZX297520V3E** platform. This chipset integrates an ARM application processor, a dedicated Cortex-M preloader subsystem, and a multi-mode LTE Cat4 baseband DSP into a single system-on-chip (SoC).

Because there is currently no standalone third-party custom firmware tree specifically labeled "ZTE K12", the most effective and reliable strategy is **stock firmware analysis, privilege escalation (via goform/WebUI vulnerabilities or debug modes), live firmware dumping, and modified-stock tailoring**. This approach preserves the proprietary ZTE cellular baseband stack (`zte_mainctrl`, `at_ctl`, DSP microcode) while unlocking critical features:
* **Cell Locking** (LTE EARFCN + PCI / Cell-ID fixation)
* **Band Locking & Aggregation control**
* **RF Reset & Carrier Re-selection automation**
* **Root Shell Access (Telnet/ADB/Dropbear/SSH)**
* **De-branding & Customization**

---

## 2. Hardware & Chipset Architecture

### 2.1 Core Subsystems
| Subsystem | Architecture / Core | Purpose | Notes |
| :--- | :--- | :--- | :--- |
| **AP (Application Processor)** | ARM Cortex-A53 (ARMv8 / AArch32 mode) | Runs Linux kernel, Busybox, GoAhead Web Server, routing daemons | Memory base typically `0x21007fc0` / `0x27ef0000` |
| **Boot MCU (Preloader)** | ARM Cortex-M0 / M-series | Inits DRAM controller, checks boot straps / USB download mode, kicks AP | Runs `zloader` (Flash) or `tloader` (USB) |
| **Baseband DSP & Modem Subsystem** | Proprietary ZTE Baseband Core | Handles LTE PHY/MAC, RF transceiver control, AT command parser | Controlled via internal IPC (`zte_mainctrl`, `at_ctl`) |
| **Flash Memory** | SPI NAND Flash (typically 128MB / 1Gb, e.g., WSON8 Dosilicon DS35M1GA / Winbond W25N01GW) | Stores bootloaders, kernels, rootfs, NVRAM, user data | Page size: 2048B + 64B/128B OOB |
| **DRAM** | 64MB / 128MB DDR2/DDR3 | System RAM | Initialized by Stage-1 loader |

### 2.2 USB Identifiers & Mode-Switching
The device supports several USB descriptor configurations governed by USB mode switching (handled by USB descriptors or WebUI commands):

| Mode | Vendor ID (VID) | Product ID (PID) | Interfaces & Endpoints | Purpose |
| :--- | :--- | :--- | :--- | :--- |
| **Initial / CD-ROM Mode** | `0x19d2` | `0x0016` / `0x1405` | Mass Storage (CD-ROM emulation / `DEMOMODEM`) | Presents PC driver installer & autorun |
| **Normal Router Mode** | `0x19d2` | `0x1405` / `0x1403` | USB NCM / CDC-ECM Ethernet (`enX`), Mass Storage | Default router operating mode |
| **Debug / Factory Mode** | `0x19d2` | `0x0500` / `0x1405` | NCM + ADB + Diag Serial (`/dev/ttyUSB0`) + AT Serial (`/dev/ttyUSB1`) | Diagnostic, AT terminal, and ADB debugging |
| **USB Download (BootROM)** | `0x19d2` | `0x0256` | Single Bulk IN/OUT endpoint pair | Low-level ROM recovery & RAM loader injection |

---

## 3. Boot Sequence & Memory Layout

```mermaid
flowchart TD
    A[Power On / Reset] --> B{BOOT pin shorted / USB dl?}
    B -- Yes (VID 0x19d2, PID 0x0256) --> C[BootROM USB Download Protocol]
    C --> D[zx297520v3-loader / Joselito Exploit]
    D --> E[RAM Bootloader U-Boot: 0x27ef0000]
    
    B -- No (Normal Boot) --> F[SPI NAND: Stage-1 zloader]
    F --> G[Initialize DDR RAM]
    G --> H[Stage-2 U-Boot in Flash]
    H --> I[Load Linux Kernel: 0x21007fc0]
    I --> J[Mount Filesystems: SquashFS / YAFFS2 / UBIFS]
    J --> K[Launch Daemons: zte_mainctrl, at_ctl, goahead WebUI]
```

### 3.1 Partition Layout (SPI NAND)
Extracted from ZX297520V3 / ZLT S10 flash structures:

| Partition Name | Typical Size / Offset | File System / Type | Content Description |
| :--- | :--- | :--- | :--- |
| `zloader` | `0x00000000` (First blocks) | Raw Binary | Cortex-M0 DRAM init preloader |
| `uboot` | Follows zloader | Raw Binary | Main U-Boot stage-2 bootloader |
| `uboot-mirr` | Mirror block | Raw Binary | Redundant backup copy of U-Boot |
| `nvrofs` | Read-only partition | Read-Only FS / Binary | Factory calibration, default IMEI (`0x0`), MAC (`0x7C`, `0x2C0`), RF tables |
| `imagefs` | Kernel image | Raw / FIT / uImage | Linux kernel (`zImage`) and AP baseband firmware |
| `rootfs` | Primary system | SquashFS / CramFS | Core Linux userspace binaries, busybox, web server |
| `yaffs` / `userdata` | Writable storage | YAFFS2 / JFFS2 / UBIFS | Dynamic configs (`/yaffs/apply_config.conf`, `/etc_rw/nv/`) |

---

## 4. Modem Inter-Process Communication (IPC) & RF Control

In ZTE ZX297520 platforms, the web server (GoAhead) does not talk directly to the modem hardware. Instead, requests flow through an internal IPC message bus:

```
[Web Client / Script]
         │
         ▼  HTTP POST /goform/goform_set_cmd_process
[GoAhead Web Server]
         │
         ▼  Internal IPC Message (e.g. 0x1527, 0x100b)
[zte_mainctrl / at_ctl]
         │
         ▼  AT Commands (e.g. AT+ZLTELC, AT+ZBAND)
[Baseband DSP / Cellular Firmware]
```

### 4.1 Key IPC Handlers Identified in `goform`
* **`0x1527` (Cell Locking / `GOFORM_LOCK_FREQUENCY`)**: Converts `actionlte`, `uarfcnlte` (EARFCN), and `callParaIdlte` (PCI) parameters into `AT+ZLTELC` commands sent to the modem baseband.
* **`0x100b` (MAC / IMEI Configuration)**: Hits `MSG_CMD_WEB_MAC_REQ` / IMEI updater routines.
* **`0x101a` (Time / Management Control)**: Triggers internal management routines in `zte_mainctrl`.
* **`0x1542` (Network Unlock)**: Processes network unlock codes and PLMN restriction bypasses.
* **`0x159a` / `0x159c` (Network Connect / Disconnect)**: Connects and disconnects LTE data bearers (useful for clean RF renegotiation and IP re-assignment).

---

## 5. Practical Roadmap for ZTE K12

1. **Step 1: WebUI & goform Probing**
   - Probe device identity (`wa_inner_version`, `cr_version`, `hardware_version`, `modem_msn`).
   - Test pre-auth and post-auth `goform` APIs for status queries and configuration extraction.

2. **Step 2: Gaining Shell / Privilege Escalation**
   - Attempt `GOFORM_REMOVE_WHITE_SITE` command injection (`ids="test\" ; telnetd -l /bin/ash #"`) or `TZ_CMD_SECURE_LOGIN` (enabling `telnetdEnable` / `adbEnable`).
   - If WebUI is locked down, use pre-auth file traversal (`/cgi-bin/zte_httpshare/`) to retrieve `/etc_rw/nv/backup` and extract `admin_Password`.

3. **Step 3: Stock Firmware Dumping**
   - With shell access: dump `/dev/mtd*` or `/dev/block/mtdblock*` directly to `/tmp` and retrieve via HTTP or Netcat.
   - Without shell access: use the `zx297520v3-loader` protocol over USB (`0x19d2:0x0256`) to boot RAM U-Boot and read flash via U-Boot memory commands (`compat_read` / `nand dump`).

4. **Step 4: Customization & Persistent Cell Locking**
   - Script direct calls to `LOCK_FREQUENCY` / `AT+ZLTELC` for automated band/cell locking without reflashing.
   - Customize `/yaffs/apply_config.conf` and `/etc_ro/default/default_parameter_sys` for persistent unlocks and custom band profiles.
