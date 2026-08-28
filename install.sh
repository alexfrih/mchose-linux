#!/usr/bin/env bash
# Build mchose and its window, put both on PATH, install the udev rule.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

cargo build --release --features gui --manifest-path "$here/Cargo.toml"
mkdir -p "$HOME/.local/bin"
ln -sf "$here/target/release/mchose" "$HOME/.local/bin/mchose"
ln -sf "$here/target/release/mchose-gui" "$HOME/.local/bin/mchose-gui"
echo "installed mchose and mchose-gui in $HOME/.local/bin"

mkdir -p "$HOME/.local/share/applications"
cp "$here/mchose.desktop" "$HOME/.local/share/applications/mchose.desktop"
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true

if ! cmp -s "$here/70-mchose.rules" /etc/udev/rules.d/70-mchose.rules; then
  sudo cp "$here/70-mchose.rules" /etc/udev/rules.d/70-mchose.rules
  sudo udevadm control --reload
  sudo udevadm trigger
  echo "installed /etc/udev/rules.d/70-mchose.rules"
fi

"$HOME/.local/bin/mchose" devices
