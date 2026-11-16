#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

mkdir -p "$HOME/.local/bin"
mkdir -p "$HOME/.config/systemd/user"
mkdir -p "$HOME/.local/share/applications"

install -m 755 "$SCRIPT_DIR/bin/vlc-hold-speed.py" "$HOME/.local/bin/vlc-hold-speed.py"
install -m 755 "$SCRIPT_DIR/bin/vlc" "$HOME/.local/bin/vlc"
install -m 755 "$SCRIPT_DIR/bin/touch-scroll" "$HOME/.local/bin/touch-scroll"
install -m 644 "$SCRIPT_DIR/systemd/vlc-hold-speed.service" "$HOME/.config/systemd/user/vlc-hold-speed.service"

if [ -f "/usr/share/applications/vlc.desktop" ]; then
    sed 's|Exec=/usr/bin/vlc|Exec='"$HOME"'/.local/bin/vlc|g' /usr/share/applications/vlc.desktop > "$HOME/.local/share/applications/vlc.desktop"
    update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
fi

systemctl --user daemon-reload
systemctl --user enable --now vlc-hold-speed.service
systemctl --user restart touch-scroll.service 2>/dev/null || true

echo "Installation complete."
