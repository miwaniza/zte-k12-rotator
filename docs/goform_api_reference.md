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
