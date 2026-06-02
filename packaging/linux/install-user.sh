#!/usr/bin/env bash
set -euo pipefail

src="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
bin="${HOME}/.local/bin"
share="${HOME}/.local/share/bridgeboard"
apps="${HOME}/.local/share/applications"
autostart="${HOME}/.config/autostart"
icons="${HOME}/.local/share/icons/hicolor/256x256/apps"

mkdir -p "${bin}" "${share}/examples" "${apps}" "${autostart}" "${icons}"

install -m 0755 "${src}/bridgeboard" "${bin}/bridgeboard"
install -m 0755 "${src}/bridgeboard-ui" "${bin}/bridgeboard-ui"
if [[ -f "${src}/bridgeboard-tray" ]]; then
    install -m 0755 "${src}/bridgeboard-tray" "${bin}/bridgeboard-tray"
elif [[ -f "${src}/bridgeboard-tray.py" ]]; then
    install -m 0755 "${src}/bridgeboard-tray.py" "${bin}/bridgeboard-tray"
else
    echo "Missing bridgeboard-tray binary" >&2
    exit 1
fi
if [[ -f "${src}/bridgeboard-tray.py" ]]; then
    install -m 0755 "${src}/bridgeboard-tray.py" "${share}/bridgeboard-tray.py"
fi

if [[ -f "${src}/bridgeboard.png" ]]; then
    install -m 0644 "${src}/bridgeboard.png" "${icons}/bridgeboard.png"
fi

if [[ -d "${src}/examples" ]]; then
    cp -f "${src}"/examples/* "${share}/examples/"
fi

desktop_source="${src}/bridgeboard-tray.desktop"
desktop_target="${apps}/bridgeboard-tray.desktop"
sed \
    -e "s|^Exec=.*|Exec=${bin}/bridgeboard-tray|" \
    -e "s|^Icon=.*|Icon=bridgeboard|" \
    "${desktop_source}" > "${desktop_target}"
chmod 0644 "${desktop_target}"
cp -f "${desktop_target}" "${autostart}/bridgeboard-tray.desktop"

"${bin}/bridgeboard" registry export --json
