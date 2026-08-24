# ZTE ZX297520 / K12 GoForm API Reference

The GoAhead web backend on ZX297520 devices provides two primary endpoints:
* **`GET /goform/goform_get_cmd_process`** (Query parameters / status)
* **`POST /goform/goform_set_cmd_process`** (Mutate parameters / trigger actions / IPC messages)

---

## 1. Information Discovery & Status Queries

### 1.1 Device Identity & Firmware Version
**Request**:
```http
GET /goform/goform_get_cmd_process?cmd=wa_inner_version,cr_version,hardware_version,modem_msn,imei,m_imei,network_type,network_provider HTTP/1.1
Host: <ROUTER_IP>
```

**Common Parameters**:
| Parameter | Description |
| :--- | :--- |
| `wa_inner_version` | Internal WebUI & Software Build String (e.g. `BD_K12V1.0.0B01`) |
| `cr_version` | Customer Release Version |
| `hardware_version` | Hardware Revision (e.g. `K12-V1.0` / `ZX297520V3_MB_V1.1`) |
| `modem_msn` | Baseband Serial Number |
| `imei` / `m_imei` | Cellular Modem IMEI |
| `network_type` | Current Network Technology (LTE, WCDMA, GSM) |
| `network_provider`| Current PLMN / Carrier Name |

### 1.2 Cellular Signal & Current Cell Information
**Request**:
```http
GET /goform/goform_get_cmd_process?cmd=lte_rsrp,lte_rsrq,lte_snr,lte_pci,lte_earfcn,cell_id,lte_ca_pcell_band,lte_ca_scell_band,rscp,ecio HTTP/1.1
Host: <ROUTER_IP>
```

**Common Parameters**:
| Parameter | Description |
| :--- | :--- |
| `lte_pci` | Current Physical Cell ID (PCI) |
| `lte_earfcn` | Current Downlink EARFCN channel number |
| `cell_id` | Global E-UTRAN Cell Identifier (ECI / eNodeB ID + Sector) |
| `lte_rsrp` / `lte_rsrq` / `lte_snr` | Radio signal metrics |
| `lte_ca_pcell_band` | LTE Primary Carrier Band |
| `lte_ca_scell_band` | LTE Secondary Carrier Band (Carrier Aggregation) |

---

## 2. RF, Band, and Cell Control Commands

### 2.1 LTE Cell Locking (`LOCK_FREQUENCY`)
Locks the cellular modem to a specific Downlink EARFCN and Cell ID (PCI).
Under the hood, `zte_mainctrl` / `at_ctl` forwards this as `AT+ZLTELC`.

**Request**:
```http
POST /goform/goform_set_cmd_process HTTP/1.1
Host: <ROUTER_IP>
Content-Type: application/x-www-form-urlencoded; charset=UTF-8

isTest=false&goformId=LOCK_FREQUENCY&actionlte=1&uarfcnlte=<EARFCN>&callParaIdlte=<PCI>
```

* `actionlte=1`: Enable cell lock on specified EARFCN + PCI.
* `actionlte=0`: Unlock / return to automatic cell selection.
* `uarfcnlte`: LTE EARFCN (e.g., `1650` for Band 3, `3000` for Band 7, `6300` for Band 20).
* `callParaIdlte`: Physical Cell ID (0 - 503).

### 2.2 Network Bearer & Band Preference (`SET_BEARER_PREFERENCE`)
**Request**:
```http
POST /goform/goform_set_cmd_process HTTP/1.1
Host: <ROUTER_IP>
Content-Type: application/x-www-form-urlencoded; charset=UTF-8

isTest=false&goformId=SET_BEARER_PREFERENCE&BearerPreference=Only_LTE&pre_mode=NET_AUTO
```

* `BearerPreference`: `Only_LTE`, `Only_WCDMA`, `Only_GSM`, `AUTO`

### 2.3 LTE Radio Re-attachment / RF Reset (`DISCONNECT_NETWORK` & `CONNECT_NETWORK`)
Forces the cellular baseband to drop and re-attach to the network (useful for forcing fresh IP lease and RF bearer negotiation after changing cell lock):

**Disconnect**:
```http
POST /goform/goform_set_cmd_process HTTP/1.1
Host: <ROUTER_IP>
Content-Type: application/x-www-form-urlencoded; charset=UTF-8

isTest=false&goformId=DISCONNECT_NETWORK
```

**Connect**:
```http
POST /goform/goform_set_cmd_process HTTP/1.1
Host: <ROUTER_IP>
Content-Type: application/x-www-form-urlencoded; charset=UTF-8

isTest=false&goformId=CONNECT_NETWORK
```

---

## 6. APN Profiles (`APN_PROC_EX`)

Recovered from the device's own WebUI bundle (`service.js`) on
`BD_MACTEXPKMF920UV1.0.0B01`, not guessed. Which goformId applies depends on the
WebUI config flag `USE_IPV6_INTERFACE`: `APN_PROC_EX` when set, `APN_PROC`
otherwise. `cmd=apn_interface_version` reports `2` on this unit.

**`apn_action` is `save`, not `set`.** `set` is rejected with `result=failure`.

### 6.1 Write a profile into a slot

```http
POST /goform/goform_set_cmd_process
isTest=false&goformId=APN_PROC_EX&apn_action=save&apn_mode=manual
&profile_name=<name>&wan_dial=*99#&apn_select=manual
&pdp_type=IP&pdp_select=auto&pdp_addr=&index=<slot>
&wan_apn=<apn>&ppp_auth_mode=none&ppp_username=&ppp_passwd=
&dns_mode=auto&prefer_dns_manual=&standby_dns_manual=
```

`pdp_type=IPv6` swaps the last two lines for their `ipv6_`-prefixed equivalents
(`ipv6_wan_apn`, `ipv6_ppp_auth_mode`, …); `IPv4v6` sends both sets.

### 6.2 Select it

Writing a profile does **not** activate it:

```http
isTest=false&goformId=APN_PROC_EX&apn_action=set_default&apn_mode=manual
&set_default_flag=1&pdp_type=IP&index=<slot>
```

### 6.3 Delete

```http
isTest=false&goformId=APN_PROC_EX&apn_action=delete&apn_mode=manual&index=<slot>
```

### 6.4 Reading the active profile

The APN is in **`wan_apn`**, with the profile name in `m_profile_name`.
`apn_name`, `m_apn_name` and `profile_name` are all empty on this firmware, so
reading only those makes a configured modem look unconfigured.

`apn_mode=auto` lets the modem pick from a built-in IMSI-keyed table, which can
hold retired carrier profiles — an MF920U with a Kyivstar SIM selected
`www.djuice.com.ua` (a brand withdrawn years ago) and the carrier refused the PDP
context. Pin the profile with `apn_mode=manual` to stop that.

### 6.5 Dial mode

```http
isTest=false&goformId=SET_CONNECTION_MODE&ConnectionMode=auto_dial   # or manual_dial
```

Confirmed working on this firmware; read it back from `dial_mode`.
