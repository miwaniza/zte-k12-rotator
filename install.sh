#!/usr/bin/env bash
set -euo pipefail

echo "=================================================================="
echo "   🚀 ZTE K12 Mobile Controller & IP Rotator - macOS / Linux Installer"
echo "=================================================================="

INSTALL_DIR="$HOME/.zte-k12-rotator"
mkdir -p "$INSTALL_DIR"

echo "[*] Downloading latest zte-control binary from GitHub Release..."
OS="$(uname -s)"
if [ "$OS" = "Darwin" ]; then
    curl -fsSL "https://github.com/miwaniza/zte-k12-rotator/releases/download/v1.0.0/zte-control" -o "$INSTALL_DIR/zte-control"
else
    curl -fsSL "https://github.com/miwaniza/zte-k12-rotator/releases/download/v1.0.0/zte-control" -o "$INSTALL_DIR/zte-control"
fi
chmod +x "$INSTALL_DIR/zte-control"

# Create symlink in /usr/local/bin or ~/.local/bin
if [ -d "$HOME/.local/bin" ]; then
    ln -sf "$INSTALL_DIR/zte-control" "$HOME/.local/bin/zte-control"
elif [ -w "/usr/local/bin" ]; then
    ln -sf "$INSTALL_DIR/zte-control" "/usr/local/bin/zte-control"
fi

echo "=================================================================="
echo "   ✅ Installation complete! Binary installed at $INSTALL_DIR/zte-control"
echo "   🚀 Start Web Dashboard: $INSTALL_DIR/zte-control ui"
echo "=================================================================="
"$INSTALL_DIR/zte-control" ui &
