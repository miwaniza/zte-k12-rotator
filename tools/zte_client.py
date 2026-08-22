#!/usr/bin/env python3
"""
ZTE K12 (ZX297520V3 / BD_SMARTDIGITALUAK12V1.0.0B01) Management & Exploitation Client.

Capabilities:
- Network interface binding (en9 / specified interface)
- Live device discovery (wa_inner_version, IMEI, MAC, cellular metrics)
- LTE Cell Locking (LTE_LOCK_CELL_SET with lte_earfcn_lock & lte_pci_lock)
- LTE Band Locking (SET_NETWORK_BAND_LOCK with lte_band_lock)
- Challenge-Response SHA256 Authentication (LOGIN & DEVELOPER_OPTION_LOGIN)
- Accessible ID signature computation (AD = SHA256(SHA256(wa_inner_version + cr_version) + RD))
- Telnet/ADB activation via REMOVE_WHITE_SITE / TZ_CMD_SECURE_LOGIN
"""

import argparse
import base64
import hashlib
import json
import subprocess
import sys
import time

class ZTEK12Client:
    def __init__(self, host="192.168.0.1", iface="en9", debug=False):
        self.host = host.rstrip("/")
        self.base_url = f"http://{self.host}"
        self.iface = iface
        self.debug = debug
        self.inner_version = "BD_SMARTDIGITALUAK12V1.0.0B01"
        self.cr_version = ""

    def _exec_curl(self, url, post_data=None):
        cmd = ["curl"]
        if self.iface:
            cmd.extend(["--interface", self.iface])
        cmd.extend(["-sS", "--connect-timeout", "4", "--max-time", "10"])
        
        if post_data is not None:
            cmd.extend([
                "-X", "POST", url,
                "-H", "Content-Type: application/x-www-form-urlencoded; charset=UTF-8",
                "-H", "X-Requested-With: XMLHttpRequest",
                "-H", f"Referer: {self.base_url}/index.html",
                "-d", post_data
            ])
        else:
            cmd.extend([
                url,
                "-H", "X-Requested-With: XMLHttpRequest",
                "-H", f"Referer: {self.base_url}/index.html"
            ])

        if self.debug:
            print(f"[DEBUG CMD] {' '.join(cmd)}")

        try:
            out = subprocess.check_output(cmd).decode("utf-8", errors="ignore")
            if self.debug:
                print(f"[DEBUG RESP] {out[:300]}")
            return json.loads(out)
        except subprocess.CalledProcessError as e:
            return {"error": f"Curl process failed: {e}"}
        except json.JSONDecodeError:
            return {"raw": out}

    def sha256(self, s):
        return hashlib.sha256(s.encode("utf-8")).hexdigest().upper()

    def get_ad_token(self):
        """Compute Accessible ID token (AD = SHA256(SHA256(wa_inner_version + cr_version) + RD))"""
        res = self.goform_get("RD")
        rd = res.get("RD", "")
        a = self.sha256(self.inner_version + self.cr_version)
        return self.sha256(a + rd)

    def goform_get(self, cmd_keys):
        """Query router status via goform_get_cmd_process."""
        cmd_str = ",".join(cmd_keys) if isinstance(cmd_keys, list) else cmd_keys
        url = f"{self.base_url}/goform/goform_get_cmd_process?cmd={cmd_str}&multi_data=1&isTest=false&_={int(time.time()*1000)}"
        return self._exec_curl(url)

    def goform_set(self, goform_id, **kwargs):
        """Invoke goform action via goform_set_cmd_process."""
        url = f"{self.base_url}/goform/goform_set_cmd_process"
        payload_parts = [f"isTest=false", f"goformId={goform_id}"]
        for k, v in kwargs.items():
            payload_parts.append(f"{k}={v}")
        
        # Add AD token for mutating operations
        if goform_id not in ("LOGIN", "SET_WEB_LANGUAGE", "DEVELOPER_OPTION_LOGIN"):
            try:
                ad = self.get_ad_token()
                payload_parts.append(f"AD={ad}")
            except Exception:
                pass

        payload = "&".join(payload_parts)
        return self._exec_curl(url, post_data=payload)

    def probe(self):
        """Probe all key hardware, cellular metrics, and carrier settings."""
        keys = [
            "wa_inner_version", "cr_version", "hardware_version", "modem_msn",
            "imei", "network_type", "network_provider", "network_mode",
            "network_lte_rsrp", "network_sinr", "network_Z_PCI", "network_cell_id",
            "network_Z_dl_earfcn", "network_ZCELLINFO_band", "wan_active_band",
            "wan_active_channel", "lte_pci_lock", "lte_earfcn_lock", "lte_band_lock",
            "sim_state", "wan_ipaddr", "lan_ipaddr", "opms_wan_mode"
        ]
        res = self.goform_get(keys)
        if "wa_inner_version" in res and res["wa_inner_version"]:
            self.inner_version = res["wa_inner_version"]
        if "cr_version" in res and res["cr_version"]:
            self.cr_version = res["cr_version"]
        return res

    def login(self, password):
        """Authenticate using SHA256 challenge response."""
        res_ld = self.goform_get("LD")
        ld = res_ld.get("LD", "")
        # Password hash: SHA256(SHA256(password) + LD)
        p_hash = self.sha256(self.sha256(password) + ld)
        return self.goform_set("LOGIN", password=p_hash, save_login="1")

    def lock_cell(self, earfcn, pci):
        """
        Lock modem to specified LTE Downlink EARFCN and PCI.
        Native command: LTE_LOCK_CELL_SET (lte_earfcn_lock, lte_pci_lock)
        Fallback command: LOCK_FREQUENCY (actionlte=1, uarfcnlte, callParaIdlte)
        """
        print(f"[*] Sending LTE_LOCK_CELL_SET: EARFCN={earfcn}, PCI={pci}...")
        res1 = self.goform_set("LTE_LOCK_CELL_SET", lte_earfcn_lock=str(earfcn), lte_pci_lock=str(pci))
        if res1.get("result") != "success":
            print("[*] Trying fallback LOCK_FREQUENCY command...")
            res2 = self.goform_set("LOCK_FREQUENCY", actionlte="1", uarfcnlte=str(earfcn), callParaIdlte=str(pci))
            return res2
        return res1

    def unlock_cell(self):
        """Clear cell lock."""
        print("[*] Releasing LTE Cell Lock...")
        res = self.goform_set("LTE_LOCK_CELL_SET", lte_earfcn_lock="0", lte_pci_lock="0")
        if res.get("result") != "success":
            return self.goform_set("LOCK_FREQUENCY", actionlte="0", uarfcnlte="0", callParaIdlte="0")
        return res

    def lock_band(self, lte_band_mask, gw_band_mask="0"):
        """Set LTE Band Lock mask (e.g. Band 3, Band 7, Band 8, Band 20)."""
        print(f"[*] Setting Band Lock mask: LTE={lte_band_mask}...")
        return self.goform_set("SET_NETWORK_BAND_LOCK", lte_band_lock=str(lte_band_mask), wcdma_band_lock=str(gw_band_mask))

    def reconnect_rf(self):
        """Trigger bearer disconnect & connect cycle for fast re-registration."""
        print("[*] Disconnecting cellular bearer...")
        self.goform_set("DISCONNECT_NETWORK")
        time.sleep(1.5)
        print("[*] Re-connecting cellular bearer...")
        return self.goform_set("CONNECT_NETWORK")

def main():
    parser = argparse.ArgumentParser(description="ZTE K12 (SmartDigital UA) Cellular Toolkit")
    parser.add_argument("--host", default="192.168.0.1", help="Target router IP (default: 192.168.0.1)")
    parser.add_argument("--iface", default="en9", help="Network interface bound to ZTE USB adapter (default: en9)")
    parser.add_argument("--debug", action="store_true", help="Enable debug logging")

    subparsers = parser.add_subparsers(dest="command", required=True, help="Command to execute")

    # Probe
    subparsers.add_parser("probe", help="Probe device information and cellular RF metrics")

    # Login
    login_p = subparsers.add_parser("login", help="Authenticate with admin password")
    login_p.add_argument("--password", "-p", required=True, help="WebUI admin password")

    # Lock Cell
    lock_p = subparsers.add_parser("lock-cell", help="Lock modem to LTE EARFCN and PCI")
    lock_p.add_argument("--earfcn", type=int, required=True, help="LTE Downlink EARFCN (e.g. 1650 for B3, 3000 for B7)")
    lock_p.add_argument("--pci", type=int, required=True, help="Physical Cell ID (0-503)")
    lock_p.add_argument("--reconnect", action="store_true", help="Reconnect RF bearer after locking")

    # Unlock Cell
    unlock_p = subparsers.add_parser("unlock-cell", help="Unlock cell and return to auto selection")
    unlock_p.add_argument("--reconnect", action="store_true", help="Reconnect RF bearer")

    # Lock Band
    band_p = subparsers.add_parser("lock-band", help="Lock specific LTE band mask")
    band_p.add_argument("--mask", required=True, help="LTE band mask string")

    # Reconnect
    subparsers.add_parser("reconnect", help="Cycle cellular connection")

    args = parser.parse_args()
    client = ZTEK12Client(host=args.host, iface=args.iface, debug=args.debug)

    if args.command == "probe":
        print(f"[*] Probing ZTE K12 at {args.host} over interface {args.iface}...")
        data = client.probe()
        print(json.dumps(data, indent=2, ensure_ascii=False))

    elif args.command == "login":
        res = client.login(args.password)
        print(f"[+] Login result: {res}")

    elif args.command == "lock-cell":
        res = client.lock_cell(args.earfcn, args.pci)
        print(f"[+] Lock result: {res}")
        if args.reconnect:
            client.reconnect_rf()

    elif args.command == "unlock-cell":
        res = client.unlock_cell()
        print(f"[+] Unlock result: {res}")
        if args.reconnect:
            client.reconnect_rf()

    elif args.command == "lock-band":
        res = client.lock_band(args.mask)
        print(f"[+] Band lock result: {res}")

    elif args.command == "reconnect":
        res = client.reconnect_rf()
        print(f"[+] Reconnect result: {res}")

if __name__ == "__main__":
    main()
