# ZTE K12 Firmware Extraction, Modification & Recovery Strategy

## 1. Overview

Modifying firmware on the ZX297520 platform is best executed in three incremental tiers:
1. **Tier 1: Userspace / Live Shell Extraction** (Safest, non-invasive)
2. **Tier 2: USB BootROM Download Mode (`zx297520v3-loader`)** (RAM U-Boot, no physical flash writes)
3. **Tier 3: In-Circuit / Out-of-Circuit SPI NAND Reading** (Hardware fallback)

---

## 2. Tier 1: Live Shell Extraction (Post-Exploit)

Once a root shell is obtained (via Telnet, ADB, or GoForm injection):

### 2.1 Identify Flash Partitions
```bash
cat /proc/mtd
# Output typically includes:
# mtd0: zloader
# mtd1: uboot
# mtd2: uboot-mirr
# mtd3: nvrofs
# mtd4: imagefs
# mtd5: rootfs
# mtd6: userdata / yaffs
```

### 2.2 Dump All Partitions
```bash
mkdir -p /tmp/dump
for i in $(cat /proc/mtd | grep -E 'mtd[0-9]+' | awk '{print $1}' | tr -d ':'); do
    echo "Dumping $i..."
    dd if=/dev/${i} of=/tmp/dump/${i}.bin bs=64k
done
```

### 2.3 Exfiltrate Dumps
* **Option A: Via Built-in Web Server**:
  ```bash
  cp -r /tmp/dump /www/
  # Then download from host PC: http://<ROUTER_IP>/dump/mtd0.bin ...
  ```
* **Option B: Netcat Transfer**:
  ```bash
  # On Host PC:
  nc -l -p 9999 > full_dump.tar.gz
  
  # On Router:
  tar -czf - /tmp/dump | nc <HOST_IP> 9999
  ```

---

## 3. Tier 2: USB BootROM Download Protocol (`zx297520v3-loader`)

When the device is powered on while shorting the `BOOT` test point (or when triggered by a low-level download command):
* The BootROM exposes USB `19d2:0256`.
* `zx297520v3-loader` implements the proprietary handshake and payload upload:
  - **Handshake**: `0x5A` -> `0xA5`
  - **Stage 1 (tloader)**: Inits DRAM and returns `0xA7`.
  - **Stage 2 (U-Boot)**: Uploads RAM U-Boot to `0x27ef0000` and executes `jumpout` (`0x8A`).
  - **Secure Boot Bypass (Joselito / CVE-2026-40003)**: Overcomes signature verification on locked devices without burning eFuses.

In the resulting RAM U-Boot shell:
```text
ZTX# printenv
ZTX# nand info
ZTX# tftp 0x21000000 ...
```

---

## 4. Tier 3: Hardware SPI NAND Flash Interfacing

### 4.1 Flash Specifications
* **Chip**: Dosilicon DS35M1GA-IB / Winbond W25N01GW (1.8V SPI NAND Flash, 1Gb / 128MB)
* **Package**: WSON-8 (8-pad surface mount)
* **Voltage**: **1.8V strictly required** (Requires 1.8V level shifter adapter when using 3.3V/5V programmers like CH341A).

### 4.2 Pinout (WSON-8)
```
          ┌───┬───┐
    /CS  1│ o │   │8  VCC (1.8V)
     SO  2│   │   │7  /HOLD
    /WP  3│   │   │6  SCK
    GND  4│   │   │5  SI
          └───┴───┘
```

---

## 5. Unpacking & Modifying Partitions

### 5.1 Stripping Out-of-Band (OOB) ECC Data
SPI NAND dumps contain 64 or 128 bytes of Spare/OOB data per 2048-byte page. Use the bundled `zlt-src/src/dump_parser.py` or `tools/dump_parser.py` to extract pure filesystem images:
```bash
python3 tools/dump_parser.py -i raw_nand_dump.bin -o unpacked_partitions/
```

### 5.2 Modifying RootFS & Configuration
1. Unsquash RootFS:
   ```bash
   unsquashfs -d rootfs_unpacked rootfs.bin
   ```
2. Enable Persistent Telnet/ADB in `/etc/init.d/rcS` or `/etc_ro/default/default_parameter_sys`:
   ```bash
   echo "telnetd -l /bin/ash &" >> rootfs_unpacked/etc/init.d/rcS
   ```
3. Disable Band and Carrier Lockdowns in `/etc_ro/default/default_parameter_sys`:
   ```properties
   tr069_app_enable=0
   tz_lock_band_state=no
   tz_lock_plmn_state=no
   tz_lock_plmn_list=
   ```
