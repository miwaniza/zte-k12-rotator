#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN="$SCRIPT_DIR/zte-control"

if [ ! -x "$BIN" ]; then
    if command -v zte-control >/dev/null 2>&1; then
        BIN="$(command -v zte-control)"
    else
        BIN="$HOME/.zte-k12-rotator/zte-control"
    fi
fi

# 1. Execute direct rotation via HTTP API or CLI. One call only: the old code
#    used a first request as an "is the service up?" probe, which itself
#    rotated, so every run rotated twice.
NEW_IP=""
ROTATE_JSON=$(curl -s --connect-timeout 2 -X POST \
    -H "X-Requested-With: XMLHttpRequest" \
    http://127.0.0.1:8080/api/rotate 2>/dev/null || true)
# Only a verified rotation reports an address; "verified":false means the
# bearer came back but the IP is unchanged or unreadable.
if printf '%s' "$ROTATE_JSON" | grep -q '"verified":true'; then
    NEW_IP=$(printf '%s' "$ROTATE_JSON" | sed -n 's/.*"wan_ip":"\([^"]*\)".*/\1/p' || true)
fi

if [ -z "$NEW_IP" ]; then
    NEW_IP=$("$BIN" reconnect 2>&1 | sed -n 's/.*new WAN IP \([0-9.]*\).*/\1/p' || true)
fi

# 2. Fetch fresh Geo-IP details (Region, City, ISP)
GEO_JSON=$(curl -s --connect-timeout 2 "http://ip-api.com/json/?fields=query,city,regionName,isp&_=$(date +%s)" || true)
PUB_IP=$(echo "$GEO_JSON" | sed -n 's/.*"query":"\([^"]*\)".*/\1/p' || true)
CITY=$(echo "$GEO_JSON" | sed -n 's/.*"city":"\([^"]*\)".*/\1/p' || true)
REGION=$(echo "$GEO_JSON" | sed -n 's/.*"regionName":"\([^"]*\)".*/\1/p' || true)
ISP=$(echo "$GEO_JSON" | sed -n 's/.*"isp":"\([^"]*\)".*/\1/p' || true)

DISPLAY_IP="${PUB_IP:-$NEW_IP}"
DISPLAY_LOC="${REGION:-—}, ${CITY:-—}"
DISPLAY_ISP="${ISP:-—}"

# 3. Fire Native macOS Notification Banner
if [ -n "$DISPLAY_IP" ]; then
    osascript -e "display notification \"🌐 Новий IP: $DISPLAY_IP\n📍 $DISPLAY_LOC ($DISPLAY_ISP)\" with title \"📡 ZTE K12: IP змінено!\" subtitle \"Ротація успішна ✅\" sound name \"Glass\""
    echo "✅ [$(date '+%H:%M:%S')] Новий IP: $DISPLAY_IP ($DISPLAY_LOC, $DISPLAY_ISP)"
else
    osascript -e 'display notification "Сесію оновлено" with title "📡 ZTE K12 Rotator" subtitle "Ротація завершена ✅" sound name "Glass"'
    echo "✅ [$(date '+%H:%M:%S')] Сесію оновлено"
fi
