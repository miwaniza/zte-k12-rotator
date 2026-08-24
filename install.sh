#!/usr/bin/env bash
# ==================================================================
# 🚀 ZTE K12 Mobile Controller & IP Rotator - macOS & Linux Installer
# ==================================================================
set -euo pipefail

INSTALL_DIR="$HOME/.zte-k12-rotator"
REPO="miwaniza/zte-k12-rotator"
VERSION="v1.0.0"

echo ""
echo "=================================================================="
echo "   🚀 ZTE K12 Mobile Controller & IP Rotator - macOS Installer"
echo "=================================================================="
echo ""
echo "[*] Target Installation Directory: $INSTALL_DIR"
mkdir -p "$INSTALL_DIR"
mkdir -p "$HOME/.local/bin"

# 1. Stop any running instances
pkill -f "zte-control ui" >/dev/null 2>&1 || true

# 2. Download latest release binary from GitHub
echo "[*] Downloading latest universal binary from GitHub Release..."
OS="$(uname -s)"
if [ "$OS" = "Darwin" ]; then
    curl -fSL "https://github.com/$REPO/releases/download/$VERSION/zte-control-macos-universal.tar.gz" -o "$INSTALL_DIR/dist.tar.gz" 2>/dev/null || \
    curl -fSL "https://github.com/$REPO/releases/download/$VERSION/zte-control" -o "$INSTALL_DIR/zte-control"
    
    if [ -f "$INSTALL_DIR/dist.tar.gz" ]; then
        tar -xzf "$INSTALL_DIR/dist.tar.gz" -C "$INSTALL_DIR"
        rm -f "$INSTALL_DIR/dist.tar.gz"
    fi
else
    curl -fSL "https://github.com/$REPO/releases/download/$VERSION/zte-control-linux-x64.tar.gz" -o "$INSTALL_DIR/dist.tar.gz" 2>/dev/null || \
    curl -fSL "https://github.com/$REPO/releases/download/$VERSION/zte-control" -o "$INSTALL_DIR/zte-control"
    
    if [ -f "$INSTALL_DIR/dist.tar.gz" ]; then
        tar -xzf "$INSTALL_DIR/dist.tar.gz" -C "$INSTALL_DIR"
        rm -f "$INSTALL_DIR/dist.tar.gz"
    fi
fi

chmod +x "$INSTALL_DIR/zte-control"

# 3. Create rotate-and-notify.sh script
cat << 'ROTEOF' > "$INSTALL_DIR/rotate-and-notify.sh"
#!/usr/bin/env bash
set -euo pipefail
INSTALL_DIR="$HOME/.zte-k12-rotator"
BIN="$INSTALL_DIR/zte-control"

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

GEO_JSON=$(curl -s --connect-timeout 2 "http://ip-api.com/json/?fields=query,city,regionName,isp&_=$(date +%s)" || true)
PUB_IP=$(echo "$GEO_JSON" | sed -n 's/.*"query":"\([^"]*\)".*/\1/p' || true)
CITY=$(echo "$GEO_JSON" | sed -n 's/.*"city":"\([^"]*\)".*/\1/p' || true)
REGION=$(echo "$GEO_JSON" | sed -n 's/.*"regionName":"\([^"]*\)".*/\1/p' || true)
ISP=$(echo "$GEO_JSON" | sed -n 's/.*"isp":"\([^"]*\)".*/\1/p' || true)

DISPLAY_IP="${PUB_IP:-$NEW_IP}"
DISPLAY_LOC="${REGION:-—}, ${CITY:-—}"
DISPLAY_ISP="${ISP:-—}"

if [ -n "$DISPLAY_IP" ]; then
    if [ "$(uname -s)" = "Darwin" ]; then
        osascript -e "display notification \"🌐 Новий IP: $DISPLAY_IP\n📍 $DISPLAY_LOC ($DISPLAY_ISP)\" with title \"📡 ZTE K12: IP змінено!\" subtitle \"Ротація успішна ✅\" sound name \"Glass\""
    fi
    echo "✅ [$(date '+%H:%M:%S')] Новий IP: $DISPLAY_IP ($DISPLAY_LOC, $DISPLAY_ISP)"
else
    if [ "$(uname -s)" = "Darwin" ]; then
        osascript -e 'display notification "Сесію оновлено" with title "📡 ZTE K12 Rotator" subtitle "Ротація завершена ✅" sound name "Glass"'
    fi
    echo "✅ [$(date '+%H:%M:%S')] Сесію оновлено"
fi
ROTEOF
chmod +x "$INSTALL_DIR/rotate-and-notify.sh"

# 4. Create symlinks in PATH
ln -sf "$INSTALL_DIR/zte-control" "$HOME/.local/bin/zte-control"
if [ -w "/usr/local/bin" ]; then
    ln -sf "$INSTALL_DIR/zte-control" "/usr/local/bin/zte-control" 2>/dev/null || true
fi

# 5. Configure macOS Background Service (LaunchAgent)
if [ "$OS" = "Darwin" ]; then
    echo "[*] Configuring macOS Background Service (LaunchAgent auto-start on login)..."
    "$INSTALL_DIR/zte-control" service install || true
fi

# 6. Create native macOS App Bundles & Desktop shortcuts
if [ "$OS" = "Darwin" ]; then
    echo "[*] Creating macOS Desktop & Application shortcuts..."
    
    APPS_DIR="$HOME/Applications"
    DESKTOP_DIR="$HOME/Desktop"
    mkdir -p "$APPS_DIR"
    
    # A. ⚡ Ротація IP (ZTE).app
    ROT_APP_NAME="⚡ Ротація IP (ZTE).app"
    create_rot_app() {
        local target_path="$1"
        rm -rf "$target_path"
        mkdir -p "$target_path/Contents/MacOS"
        
        cat << 'APPLIST' > "$target_path/Contents/Info.plist"
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>ZTE Rotator</string>
    <key>CFBundleDisplayName</key>
    <string>⚡ Ротація IP (ZTE)</string>
    <key>CFBundleIdentifier</key>
    <string>com.zte.rotator.app</string>
    <key>CFBundleVersion</key>
    <string>1.0.0</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleExecutable</key>
    <string>run</string>
    <key>LSUIElement</key>
    <true/>
</dict>
</plist>
APPLIST

        cat << 'APPEXEC' > "$target_path/Contents/MacOS/run"
#!/usr/bin/env bash
"$HOME/.zte-k12-rotator/rotate-and-notify.sh" >/dev/null 2>&1 &
APPEXEC
        chmod +x "$target_path/Contents/MacOS/run"
    }

    create_rot_app "$APPS_DIR/$ROT_APP_NAME"
    if [ -d "$DESKTOP_DIR" ]; then
        create_rot_app "$DESKTOP_DIR/$ROT_APP_NAME"
    fi

    # B. ZTE K12 Controller.app (Dashboard)
    DASH_APP_NAME="ZTE K12 Controller.app"
    create_dash_app() {
        local target_path="$1"
        rm -rf "$target_path"
        mkdir -p "$target_path/Contents/MacOS"
        
        cat << 'APPLIST' > "$target_path/Contents/Info.plist"
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>ZTE Controller</string>
    <key>CFBundleDisplayName</key>
    <string>ZTE K12 Controller</string>
    <key>CFBundleIdentifier</key>
    <string>com.zte.controller.app</string>
    <key>CFBundleVersion</key>
    <string>1.0.0</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleExecutable</key>
    <string>run</string>
</dict>
</plist>
APPLIST

        cat << 'APPEXEC' > "$target_path/Contents/MacOS/run"
#!/usr/bin/env bash
open "http://127.0.0.1:8080"
APPEXEC
        chmod +x "$target_path/Contents/MacOS/run"
    }

    create_dash_app "$APPS_DIR/$DASH_APP_NAME"
    if [ -d "$DESKTOP_DIR" ]; then
        create_dash_app "$DESKTOP_DIR/$DASH_APP_NAME"
    fi

    echo "[+] Desktop shortcut created: ~/Desktop/$ROT_APP_NAME"
    echo "[+] Application shortcut created: ~/Applications/$ROT_APP_NAME"
fi

# 7. Shell profile aliases
SHELL_RC=""
if [ -n "${ZSH_VERSION:-}" ] || [ -f "$HOME/.zshrc" ]; then
    SHELL_RC="$HOME/.zshrc"
elif [ -f "$HOME/.bash_profile" ]; then
    SHELL_RC="$HOME/.bash_profile"
elif [ -f "$HOME/.bashrc" ]; then
    SHELL_RC="$HOME/.bashrc"
fi

if [ -n "$SHELL_RC" ]; then
    if ! grep -q "zte-k12-rotator" "$SHELL_RC" 2>/dev/null; then
        echo "" >> "$SHELL_RC"
        echo "# ZTE K12 Rotator" >> "$SHELL_RC"
        echo 'export PATH="$HOME/.local/bin:$HOME/.zte-k12-rotator:$PATH"' >> "$SHELL_RC"
        echo 'alias rotate="$HOME/.zte-k12-rotator/rotate-and-notify.sh"' >> "$SHELL_RC"
        echo 'alias zte="$HOME/.zte-k12-rotator/zte-control"' >> "$SHELL_RC"
    fi
fi

# 8. Start server and open dashboard
echo ""
echo "=================================================================="
echo "   ✅ INSTALLATION & SERVICE SETUP COMPLETE! 🎉"
echo "=================================================================="
echo ""
echo " 🌐 Web Dashboard: http://127.0.0.1:8080"
echo " ⚡ 1-Click Rotate: Double click on '⚡ Ротація IP (ZTE)' on Desktop"
echo " 💻 Terminal CLI:  rotate   OR   zte-control status"
echo ""
echo "=================================================================="

# Open dashboard in browser
if [ "$OS" = "Darwin" ]; then
    open "http://127.0.0.1:8080" || true
fi
