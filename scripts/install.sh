#!/usr/bin/env bash
set -euo pipefail
project=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
app=/opt/su3-raman
data=/var/lib/su3-raman
config=/etc/su3
binary=$project/target/release/luxos
bundle=
passphrase=
offline=0

usage() {
    printf 'Usage: %s [--binary LUXOS] [--library BUNDLE] [--setup-passphrase-file FILE]\n' "$0"
}
while (( $# )); do
    case "$1" in
        --binary) binary=${2:?}; shift 2 ;;
        --library) bundle=${2:?}; shift 2 ;;
        --setup-passphrase-file) passphrase=${2:?}; shift 2 ;;
        --image-binary) binary=${2:?}; offline=1; shift 2 ;;
        --help|-h) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
done
[[ $EUID == 0 ]] || { echo "Run the installer as root" >&2; exit 1; }
[[ $(uname -m) == aarch64 ]] || { echo "This installer requires 64-bit ARM Linux" >&2; exit 1; }
. /etc/os-release
case ${VERSION_CODENAME:-} in
    bookworm|trixie) ;;
    *) echo "Use a Bookworm or Trixie image" >&2; exit 1 ;;
esac

[[ -x "$binary" ]] || { echo "Build LuxOS with cargo build --release --locked or pass --binary" >&2; exit 1; }
"$binary" --version
if [[ -n "$bundle" && -d "$data/index" && -n $(ls -A "$data/index") ]]; then
    echo "Move the existing index aside before installing a replacement library" >&2
    exit 1
fi
if (( ! offline )); then
    apt-get update
    apt-get install -y --no-install-recommends network-manager dnsmasq-base avahi-daemon \
        ca-certificates wpasupplicant
fi
if ! getent passwd luxos >/dev/null; then
    useradd --system --user-group --no-create-home --home-dir "$data" --shell /usr/sbin/nologin luxos
fi
install -d -m 0755 "$app" "$config" /etc/NetworkManager/dnsmasq-shared.d
install -d -o luxos -g luxos -m 0750 "$data" "$data/index"
stage=$(mktemp -d "$app/.install.XXXXXX")
trap 'rm -rf "$stage"' EXIT
install -m 0755 "$binary" "$stage/luxos"
mv -f "$stage/luxos" "$app/luxos"

legacy=/etc/systemd/system/su3-raman-mcp.service
if [[ -f "$legacy" ]]; then
    if (( offline )); then
        systemctl --root=/ disable su3-raman-mcp.service
    else
        systemctl disable --now su3-raman-mcp.service
    fi
    rm -f "$legacy"
fi
install -m 0644 "$project"/systemd/*.service /etc/systemd/system/
printf '%s\n' 'address=/#/10.42.77.1' > /etc/NetworkManager/dnsmasq-shared.d/10-su3-captive.conf
if [[ -n "$passphrase" ]]; then
    install -m 0600 "$passphrase" "$config/setup-passphrase"
elif [[ ! -s "$config/setup-passphrase" ]]; then
    (umask 077; "$app/luxos" setup-passphrase > "$config/setup-passphrase")
fi
if [[ -n "$bundle" ]]; then
    available=$(df -Pk "$data" | awk 'END {print $4}')
    size=$(stat -c %s "$bundle")
    if (( size > (available - 128 * 1024) * 1024 )); then
        echo "Library archive exceeds safe available space" >&2
        exit 1
    fi
    install -m 0640 -o luxos -g luxos "$bundle" "$data/library.tar.gz"
    (cd "$data"; sha256sum library.tar.gz > library.sha256)
    chown luxos:luxos "$data/library.sha256"
    chmod 0640 "$data/library.sha256"
fi

units=(su3-wifi.service su3-raman.service su3-library.service)
if (( offline )); then
    systemctl --root=/ mask dnsmasq.service
    systemctl --root=/ enable NetworkManager.service avahi-daemon.service "${units[@]}"
else
    systemctl daemon-reload
    systemctl enable --now NetworkManager.service avahi-daemon.service
    systemctl enable "${units[@]}"
    systemctl restart "${units[@]}"
fi
printf 'Hotspot passphrase saved at %s/setup-passphrase\n' "$config"
