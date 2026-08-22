#!/usr/bin/env python3
"""
ZTE K12 (SmartDigital UA / BD_SMARTDIGITALUAK12V1.0.0B01) Automated Client & Diagnostic Tool.

Features:
- Full SHA-256 Challenge-Response Authentication (LD salt + SHA256)
- Session persistence via cookies
- Baseband RF Metrics (RSRP, RSSI, SINR, RSRQ, Band, WAN IP)
- Developer Options & Tracing Tool URL generation
- Band & Cell locking queries
"""

import argparse
import hashlib
import json
import os
import subprocess
import sys
import time

COOKIE_FILE = os.path.expanduser("~/.superset/projects/zte-throttle/scratch/session_cookie.txt")

class ZTEK12Client:
    def __init__(self, host="192.168.0.1", iface="en9", cookie_file=COOKIE_FILE, debug=False):
        self.host = host.rstrip("/")
        self.base_url = f"http://{self.host}"
        self.iface = iface
        self.cookie_file = cookie_file
        self.debug = debug
        self.inner_version = "BD_SMARTDIGITALUAK12V1.0.0B01"
        self.cr_version = ""
        os.makedirs(os.path.dirname(self.cookie_file), exist_ok=True)

    def sha256_upper(self, s):
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

    def goform_get(self, cmd_keys, multi_data=True):
        cmd_str = ",".join(cmd_keys) if isinstance(cmd_keys, list) else cmd_keys
        m = "&multi_data=1" if multi_data else ""
        url = f"{self.base_url}/goform/goform_get_cmd_process?cmd={cmd_str}{m}&isTest=false&_={int(time.time()*1000)}"
        return self._exec(url)

    def login(self, password="353FALM5"):
        """Perform challenge-response login using router LD salt."""
        # 1. Fetch fresh LD salt
        res_ld = self.goform_get("LD", multi_data=False)
        ld = res_ld.get("LD", "")
        
        # 2. Compute SHA256(SHA256(pwd) + LD)
        p1 = self.sha256_upper(password)
        p_hash = self.sha256_upper(p1 + ld)

        # 3. Post login
        url = f"{self.base_url}/goform/goform_set_cmd_process"
        payload = f"isTest=false&goformId=LOGIN&password={p_hash}&save_login=1"
        res = self._exec(url, post_data=payload)
        return res

    def get_status(self):
        """Query complete cellular telemetry and system state."""
        keys = [
            "wa_inner_version", "hardware_version", "modem_msn", "imei",
            "network_type", "network_provider", "net_select_mode",
            "network_lte_rsrp", "network_sinr", "lte_rsrp", "lte_rsrq", "lte_snr", "lte_rssi",
            "wan_active_band", "wan_active_channel", "lte_pci", "lte_earfcn", "cell_id",
            "lte_band_lock", "sim_state", "wan_ipaddr", "lan_ipaddr", "opms_wan_mode",
            "loginfo", "Language", "web_version"
        ]
        res = self.goform_get(keys)
        return {k: v for k, v in res.items() if v != ""}

    def set_language(self, lang="uk"):
        """Set router WebUI language (uk / en)."""
        url = f"{self.base_url}/goform/goform_set_cmd_process"
        payload = f"isTest=false&goformId=SET_WEB_LANGUAGE&Language={lang}"
        return self._exec(url, post_data=payload)

def main():
    parser = argparse.ArgumentParser(description="ZTE K12 Router Management Suite")
    parser.add_argument("--host", default="192.168.0.1", help="Target router IP")
    parser.add_argument("--iface", default="en9", help="Interface bound to ZTE modem")
    parser.add_argument("--debug", action="store_true", help="Enable debug logs")

    subparsers = parser.add_subparsers(dest="command", required=True)

    # Login
    login_p = subparsers.add_parser("login", help="Authenticate with WebUI password")
    login_p.add_argument("--password", "-p", default="353FALM5", help="WebUI password (default: 353FALM5)")

    # Status
    subparsers.add_parser("status", help="Get full cellular RF signal & device status")

    # Set Language
    lang_p = subparsers.add_parser("set-lang", help="Set WebUI language")
    lang_p.add_argument("lang", choices=["uk", "en"], help="Language code")

    args = parser.parse_args()
    client = ZTEK12Client(host=args.host, iface=args.iface, debug=args.debug)

    if args.command == "login":
        print(f"[*] Logging in to ZTE K12 at {args.host} (via {args.iface})...")
        res = client.login(args.password)
        print(f"[+] Login Result: {json.dumps(res)}")
        if res.get("result") == "0":
            print("[+] Successfully authenticated! Session saved to cookies.")

    elif args.command == "status":
        status = client.get_status()
        if status.get("loginfo") != "ok":
            print("[*] Session expired or unauthenticated. Logging in with default password...")
            client.login()
            status = client.get_status()
        print(json.dumps(status, indent=2, ensure_ascii=False))

    elif args.command == "set-lang":
        res = client.set_language(args.lang)
        print(f"[+] Set language to {args.lang}: {json.dumps(res)}")

if __name__ == "__main__":
    main()
