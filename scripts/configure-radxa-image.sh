#!/usr/bin/env bash
set -euo pipefail
image=/work/system.img
root=/mnt/radxa
first=$(sgdisk --info=3 "$image" | awk '/First sector:/ {print $3}')
last=$(sgdisk --info=3 "$image" | awk '/Last sector:/ {print $3}')
[[ $first =~ ^[0-9]+$ && $last =~ ^[0-9]+$ && $last -gt $first ]]
mkdir "$root"
loop=$(losetup --find --show --offset "$((first * 512))" \
    --sizelimit "$(((last - first + 1) * 512))" "$image")
cleanup() {
    umount -R "$root" 2>/dev/null || true
    losetup -d "$loop"
}
trap cleanup EXIT
mount "$loop" "$root"
mount --bind /dev "$root/dev"
mount -t proc proc "$root/proc"
mkdir "$root/tmp/luxos-source" "$root/tmp/luxos-input"
mount --bind /source "$root/tmp/luxos-source"
touch "$root/tmp/luxos-input/luxos"
mount --bind /luxos-bin/luxos "$root/tmp/luxos-input/luxos"
install -m 0600 /work/setup-passphrase "$root/tmp/luxos-input/setup-passphrase"

if [[ -e "$root/etc/resolv.conf" || -L "$root/etc/resolv.conf" ]]; then
    cp -a "$root/etc/resolv.conf" /tmp/radxa-resolv
fi
rm -f "$root/etc/resolv.conf"
cp -L /etc/resolv.conf "$root/etc/resolv.conf"
if [[ -e "$root/usr/sbin/policy-rc.d" ]]; then
    cp -a "$root/usr/sbin/policy-rc.d" /tmp/radxa-policy
fi
printf '#!/bin/sh\nexit 101\n' > "$root/usr/sbin/policy-rc.d"
chmod 755 "$root/usr/sbin/policy-rc.d"

missing=()
for entry in avahi-daemon:sbin/avahi-daemon dnsmasq-base:sbin/dnsmasq \
    network-manager:bin/nmcli openssh-server:sbin/sshd sudo:bin/sudo; do
    [[ -x "$root/usr/${entry#*:}" ]] || missing+=("${entry%%:*}")
done
if (( ${#missing[@]} )); then
    printf '%s\n' 'deb https://deb.debian.org/debian bookworm main' \
        'deb https://deb.debian.org/debian bookworm-updates main' \
        'deb https://deb.debian.org/debian-security bookworm-security main' \
        > "$root/etc/apt/sources.list.d/luxos-build.list"
    opts=(-o Dir::Etc::sourcelist=sources.list.d/luxos-build.list -o Dir::Etc::sourceparts=-)
    chroot "$root" apt-get "${opts[@]}" update
    chroot "$root" env DEBIAN_FRONTEND=noninteractive NEEDRESTART_MODE=l \
        apt-get "${opts[@]}" install -y --no-install-recommends "${missing[@]}"
    chroot "$root" apt-get clean
    rm -f "$root/etc/apt/sources.list.d/luxos-build.list" "$root"/var/lib/apt/lists/*_Packages*
fi

args=(--image-binary /tmp/luxos-input/luxos --setup-passphrase-file /tmp/luxos-input/setup-passphrase)
if [[ -f /input-library.tar.gz ]]; then
    touch "$root/tmp/luxos-input/library.tar.gz"
    mount --bind /input-library.tar.gz "$root/tmp/luxos-input/library.tar.gz"
    args+=(--library /tmp/luxos-input/library.tar.gz)
fi
chroot "$root" /tmp/luxos-source/scripts/install.sh "${args[@]}"

printf 'su3-raman\n' > "$root/etc/hostname"
cat > "$root/etc/hosts" <<'HOSTS'
127.0.0.1 localhost
127.0.1.1 su3-raman
::1 localhost ip6-localhost ip6-loopback
HOSTS
chroot "$root" useradd --create-home --shell /bin/bash su3
for user in root radxa rock; do
    if chroot "$root" getent passwd "$user" >/dev/null; then
        chroot "$root" passwd -l "$user"
    fi
done
chroot "$root" install -o su3 -g su3 -m 0700 -d /home/su3/.ssh
install -m 0600 /work/authorized_keys "$root/home/su3/.ssh/authorized_keys"
chroot "$root" chown su3:su3 /home/su3/.ssh/authorized_keys
install -d "$root/etc/ssh/sshd_config.d" "$root/etc/sudoers.d" "$root/run/sshd"
cat > "$root/etc/ssh/sshd_config.d/10-su3.conf" <<'SSHD'
PermitRootLogin no
PasswordAuthentication no
KbdInteractiveAuthentication no
AllowUsers su3
SSHD
printf 'su3 ALL=(root) NOPASSWD: ALL\n' > "$root/etc/sudoers.d/90-su3"
chmod 0440 "$root/etc/sudoers.d/90-su3"
chroot "$root" visudo -cf /etc/sudoers.d/90-su3
chroot "$root" ssh-keygen -A
chroot "$root" /usr/sbin/sshd -t
rm -f "$root"/etc/ssh/ssh_host_*
cat > "$root/etc/systemd/system/su3-ssh-keys.service" <<'UNIT'
[Unit]
Description=Create SSH host keys for this LuxOS device
Before=ssh.service
ConditionPathExists=!/etc/ssh/ssh_host_ed25519_key

[Service]
Type=oneshot
ExecStart=/usr/bin/ssh-keygen -A

[Install]
WantedBy=ssh.service
UNIT
chroot "$root" systemctl --root=/ enable ssh.service su3-ssh-keys.service
chroot "$root" systemctl --root=/ set-default multi-user.target

rm -f "$root/etc/resolv.conf" "$root/usr/sbin/policy-rc.d"
if [[ -e /tmp/radxa-resolv || -L /tmp/radxa-resolv ]]; then
    mv /tmp/radxa-resolv "$root/etc/resolv.conf"
else
    ln -s ../run/NetworkManager/resolv.conf "$root/etc/resolv.conf"
fi
if [[ -e /tmp/radxa-policy ]]; then
    mv /tmp/radxa-policy "$root/usr/sbin/policy-rc.d"
fi
: > "$root/etc/machine-id"
if [[ -f /input-library.tar.gz ]]; then
    umount "$root/tmp/luxos-input/library.tar.gz"
fi
umount "$root/tmp/luxos-source" "$root/tmp/luxos-input/luxos"
rm -rf "$root/tmp/luxos-source" "$root/tmp/luxos-input"
find "$root/var/log" -type f -exec truncate -s 0 {} +
sync
umount -R "$root"
e2fsck -fn "$loop"
