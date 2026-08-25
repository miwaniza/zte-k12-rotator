#!/usr/bin/env python3
"""
ZTE K12 Easy Cell Tower & Provider Control Utility.

Provides an easy, interactive terminal interface and CLI commands to:
- Lock to specific LTE Frequency Bands (B3, B7, B8, B20)
- Lock to specific Cell Towers / Sectors (EARFCN + PCI)
- Control Cellular Network Provider / Operator (Vodafone, Kyivstar, lifecell)
- Live monitor signal strength and RF metrics
- Open direct engineering WebUI pages in browser
"""

import argparse
import hashlib
import json
import os
import subprocess
import sys
import time
import webbrowser

# Session cookies live under the user's own cache dir. This used to point into
# an unrelated project's scratch tree (~/.superset/projects/zte-throttle).
COOKIE_FILE = os.path.expanduser("~/.cache/zte-control/session_cookie.txt")

# Band masks for ZTE K12 (ZX297520 / MC888x framework)
BAND_MASKS = {
    "B3":  {"name": "Band 3 (1800 MHz)", "mask": "0x0000000000000004", "val": 0x4},
    "B7":  {"name": "Band 7 (2600 MHz)", "mask": "0x0000000000000040", "val": 0x40},
    "B8":  {"name": "Band 8 (900 MHz)",  "mask": "0x0000000000000080", "val": 0x80},
    "B20": {"name": "Band 20 (800 MHz)", "mask": "0x0000000000080000", "val": 0x80000},
}
ALL_UA_BANDS_MASK = "0x00000000000800c4"

# Ukrainian Mobile Operators
PROVIDERS = {
    "25501": "Vodafone UA",
    "25503": "Kyivstar",
    "25506": "lifecell",
    "25507": "TriMob (3Mob)",
}

class CellController:
    def __init__(self, host="192.168.0.1", iface="en9", password=""):
        self.host = host.rstrip("/")
        self.base_url = f"http://{self.host}"
        self.iface = iface
        self.password = password
        self.cookie_file = COOKIE_FILE
        self.inner_version = "BD_SMARTDIGITALUAK12V1.0.0B01"
        os.makedirs(os.path.dirname(self.cookie_file), exist_ok=True)

    def sha256(self, s):
        return hashlib.sha256(s.encode("utf-8")).hexdigest().upper()

    def _exec(self, url, post_data=None):
        cmd = ["curl"]
        if self.iface:
            cmd.extend(["--interface", self.iface])
        cmd.extend([
            "-sS", "--connect-timeout", "4", "--max-time", "10",
            "-c", self.cookie_file, "-b", self.cookie_file,
            "-H", "X-Requested-With: XMLHttpRequest",
            "-H", f"Referer: {self.base_url}/index.html"
        ])
        if post_data is not None:
            cmd.extend([
                "-X", "POST", url,
                "-H", "Content-Type: application/x-www-form-urlencoded; charset=UTF-8",
                "-d", post_data
            ])
        else:
            cmd.append(url)

        try:
            out = subprocess.check_output(cmd).decode("utf-8", errors="ignore")
            return json.loads(out)
        except subprocess.CalledProcessError as e:
            return {"error": f"Connection failed: {e}"}
        except json.JSONDecodeError:
            return {"raw": out}

    def login(self):
        """Authenticate using SHA256 challenge-response."""
        if not self.password:
            raise ValueError("a WebUI password is required (--password or ZTE_PASSWORD)")
        url_ld = f"{self.base_url}/goform/goform_get_cmd_process?cmd=LD&isTest=false&_={int(time.time()*1000)}"
        res_ld = self._exec(url_ld)
        ld = res_ld.get("LD", "") if isinstance(res_ld, dict) else ""
        
        p1 = self.sha256(self.password)
        p_hash = self.sha256(p1 + ld)

        url_login = f"{self.base_url}/goform/goform_set_cmd_process"
        payload = f"isTest=false&goformId=LOGIN&password={p_hash}&save_login=1"
        return self._exec(url_login, post_data=payload)

    def get_status(self):
        """Get live cellular signal, band, and registration status."""
        keys = [
            "wa_inner_version", "hardware_version", "modem_msn", "imei",
            "network_type", "network_provider", "net_select_mode",
            "network_lte_rsrp", "network_sinr", "lte_rsrp", "lte_rsrq", "lte_snr", "lte_rssi",
            "wan_active_band", "wan_active_channel", "lte_pci", "lte_earfcn", "cell_id",
            "lte_band_lock", "sim_state", "wan_ipaddr", "lan_ipaddr", "opms_wan_mode",
            "loginfo", "Language"
        ]
        cmd_str = ",".join(keys)
        url = f"{self.base_url}/goform/goform_get_cmd_process?cmd={cmd_str}&multi_data=1&isTest=false&_={int(time.time()*1000)}"
        res = self._exec(url)
        if not isinstance(res, dict) or res.get("loginfo") != "ok":
            self.login()
            res = self._exec(url)
        return {k: v for k, v in res.items() if v != ""} if isinstance(res, dict) else {}

    def decode_band_mask(self, mask_str):
        """Decode hex band mask into human-readable LTE bands."""
        try:
            val = int(mask_str, 16)
            bands = []
            for b_name, b_info in BAND_MASKS.items():
                if val & b_info["val"]:
                    bands.append(b_name)
            return ", ".join(bands) if bands else "Unknown"
        except Exception:
            return mask_str

    def lock_bands(self, band_list):
        """Set LTE Band Lock mask."""
        if "ALL" in [b.upper() for b in band_list]:
            mask_str = ALL_UA_BANDS_MASK
            desc = "All UA LTE Bands (B3 + B7 + B8 + B20)"
        else:
            total_val = 0
            for b in band_list:
                b_up = b.upper().strip()
                if b_up in BAND_MASKS:
                    total_val += BAND_MASKS[b_up]["val"]
                elif b_up.startswith("B") and b_up[1:] in BAND_MASKS:
                    total_val += BAND_MASKS[b_up[1:]]["val"]
            hex_str = hex(total_val)[2:]
            mask_str = "0x" + hex_str.zfill(16)
            desc = f"Bands {', '.join(band_list)} (Mask: {mask_str})"

        print(f"[*] Applying Band Lock: {desc}...")
        url = f"{self.base_url}/goform/goform_set_cmd_process"
        payload = f"isTest=false&goformId=BAND_SELECT&is_gw_band=0&gw_band_mask=0&is_lte_band=1&lte_band_mask={mask_str}"
        res = self._exec(url, post_data=payload)
        return res

    def lock_cell(self, earfcn, pci):
        """Lock modem to specific EARFCN channel and Physical Cell ID."""
        print(f"[*] Locking to Cell Tower: EARFCN={earfcn}, PCI={pci}...")
        url = f"{self.base_url}/goform/goform_set_cmd_process"
        payload = f"isTest=false&goformId=LTE_LOCK_CELL_SET&lte_earfcn_lock={earfcn}&lte_pci_lock={pci}"
        res = self._exec(url, post_data=payload)
        return res

    def unlock_cell(self):
        """Clear cell lock."""
        print("[*] Releasing Cell Lock (Auto Cell Selection)...")
        url = f"{self.base_url}/goform/goform_set_cmd_process"
        payload = f"isTest=false&goformId=LTE_LOCK_CELL_SET&lte_earfcn_lock=0&lte_pci_lock=0"
        return self._exec(url, post_data=payload)

    def reconnect_rf(self):
        """Force cellular bearer reset."""
        print("[*] Cycling cellular bearer (fast reconnect)...")
        url = f"{self.base_url}/goform/goform_set_cmd_process"
        self._exec(url, post_data="isTest=false&goformId=DISCONNECT_NETWORK")
        time.sleep(1.5)
        return self._exec(url, post_data="isTest=false&goformId=CONNECT_NETWORK")

    def open_webui(self, route="#developer_options"):
        """Open browser directly to the engineering / developer options WebUI page."""
        target_url = f"{self.base_url}/{route}"
        print(f"[*] Opening browser to: {target_url}")
        webbrowser.open(target_url)

    def interactive_menu(self):
        """Interactive terminal control dashboard."""
        while True:
            status = self.get_status()
            os.system("clear" if os.name == "posix" else "cls")
            print("=" * 65)
            print("      📡 ZTE K12 CELL TOWER & BAND CONTROL DASHBOARD")
            print("=" * 65)
            print(f" Model:         {status.get('hardware_version', 'K12')} (Firmware: {status.get('wa_inner_version', 'N/A')})")
            print(f" Network State: {status.get('network_type', 'N/A')} ({status.get('opms_wan_mode', 'PPP')})")
            
            rsrp = status.get('lte_rsrp', status.get('network_lte_rsrp', 'N/A'))
            rssi = status.get('lte_rssi', 'N/A')
            sinr = status.get('lte_snr', status.get('network_sinr', 'N/A'))
            rsrq = status.get('lte_rsrq', 'N/A')
            print(f" Signal:        RSRP: {rsrp} dBm | RSSI: {rssi} dBm | SINR: {sinr} dB | RSRQ: {rsrq} dB")
            
            band_raw = status.get('lte_band_lock', '0x0000800c4')
            band_decoded = self.decode_band_mask(band_raw)
            print(f" Active Bands:  {band_decoded} ({band_raw})")
            print("-" * 65)
            print(" [1] 🔒 Lock Band: Band 3 (1800 MHz - Urban Speed)")
            print(" [2] 🔒 Lock Band: Band 7 (2600 MHz - High Capacity)")
            print(" [3] 🔒 Lock Band: Band 8 (900 MHz - Long Range)")
            print(" [4] 🔒 Lock Band: Band 20 (800 MHz - Deep Penetration)")
            print(" [5] 🔓 Unlock All Bands (B3 + B7 + B8 + B20)")
            print(" [6] 🎯 Cell Tower Lock (Enter EARFCN + PCI)")
            print(" [7] 🔄 Reset / Clear Cell Lock")
            print(" [8] 🌐 Open Developer Options in Web Browser (#developer_options)")
            print(" [9] 📊 Open Cellular Signal Live Monitor")
            print(" [0] 🚪 Exit")
            print("=" * 65)
            
            choice = input(" Select option [0-9]: ").strip()
            if choice == "1":
                self.lock_bands(["B3"])
                input("\nPress Enter to continue...")
            elif choice == "2":
                self.lock_bands(["B7"])
                input("\nPress Enter to continue...")
            elif choice == "3":
                self.lock_bands(["B8"])
                input("\nPress Enter to continue...")
            elif choice == "4":
                self.lock_bands(["B20"])
                input("\nPress Enter to continue...")
            elif choice == "5":
                self.lock_bands(["ALL"])
                input("\nPress Enter to continue...")
            elif choice == "6":
                earfcn = input("Enter target EARFCN (e.g. 1650 for B3, 3000 for B7, 6300 for B20): ").strip()
                pci = input("Enter target PCI (Physical Cell ID 0-503): ").strip()
                if earfcn.isdigit() and pci.isdigit():
                    res = self.lock_cell(int(earfcn), int(pci))
                    print(f"Result: {res}")
                else:
                    print("[-] Invalid EARFCN or PCI.")
                input("\nPress Enter to continue...")
            elif choice == "7":
                res = self.unlock_cell()
                print(f"Result: {res}")
                input("\nPress Enter to continue...")
            elif choice == "8":
                self.open_webui("#developer_options")
                time.sleep(1)
            elif choice == "9":
                print("\nStarting live monitor (Ctrl+C to return to menu)...")
                try:
                    while True:
                        st = self.get_status()
                        ts = time.strftime("%H:%M:%S")
                        r = st.get('lte_rsrp', st.get('network_lte_rsrp', 'N/A'))
                        sn = st.get('lte_snr', st.get('network_sinr', 'N/A'))
                        rq = st.get('lte_rsrq', 'N/A')
                        print(f"[{ts}] RSRP: {r:<6} dBm | SINR: {sn:<5} dB | RSRQ: {rq:<5} dB")
                        time.sleep(2)
                except KeyboardInterrupt:
                    pass
            elif choice == "0":
                print("\nExiting dashboard.")
                break

def main():
    parser = argparse.ArgumentParser(description="ZTE K12 Cell Tower & Band Controller")
    parser.add_argument("--host", default="192.168.0.1", help="Target router IP (default: 192.168.0.1)")
    parser.add_argument("--iface", default="en9", help="Network interface (default: en9)")
    # No default: a password baked into the source is a credential shipped to
    # everyone who clones the repo.
    parser.add_argument("--password", "-p", default=os.environ.get("ZTE_PASSWORD", ""),
                        help="WebUI password (or set ZTE_PASSWORD)")

    subparsers = parser.add_subparsers(dest="command", help="Command to run")

    # Interactive Dashboard
    subparsers.add_parser("menu", help="Launch interactive terminal dashboard")

    # Status
    subparsers.add_parser("status", help="Show current signal & band status")

    # Band Lock
    band_p = subparsers.add_parser("lock-band", help="Lock specific LTE band(s)")
    band_p.add_argument("bands", nargs="+", help="Bands to lock (e.g. B3, B7, B8, B20, or ALL)")

    # Cell Lock
    cell_p = subparsers.add_parser("lock-cell", help="Lock to specific EARFCN and PCI")
    cell_p.add_argument("--earfcn", type=int, required=True, help="LTE Downlink EARFCN")
    cell_p.add_argument("--pci", type=int, required=True, help="Physical Cell ID (0-503)")
    cell_p.add_argument("--reconnect", action="store_true", help="Reconnect cellular bearer after locking")

    # Unlock Cell
    subparsers.add_parser("unlock-cell", help="Clear cell lock")

    # Open WebUI
    web_p = subparsers.add_parser("webui", help="Open WebUI Developer Options in browser")
    web_p.add_argument("--page", default="#developer_options", help="WebUI hash route (default: #developer_options)")

    # Reconnect
    subparsers.add_parser("reconnect", help="Cycle cellular connection")

    args = parser.parse_args()
    controller = CellController(host=args.host, iface=args.iface, password=args.password)

    if args.command == "menu" or len(sys.argv) == 1:
        controller.interactive_menu()

    elif args.command == "status":
        st = controller.get_status()
        print(json.dumps(st, indent=2, ensure_ascii=False))

    elif args.command == "lock-band":
        res = controller.lock_bands(args.bands)
        print(f"Result: {res}")

    elif args.command == "lock-cell":
        res = controller.lock_cell(args.earfcn, args.pci)
        print(f"Result: {res}")
        if args.reconnect:
            controller.reconnect_rf()

    elif args.command == "unlock-cell":
        res = controller.unlock_cell()
        print(f"Result: {res}")

    elif args.command == "webui":
        controller.open_webui(args.page)

    elif args.command == "reconnect":
        controller.reconnect_rf()

if __name__ == "__main__":
    main()
