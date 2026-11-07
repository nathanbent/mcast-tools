#!/usr/bin/env bash
# install.sh - turn a fresh Raspberry Pi OS Lite (64-bit) install into an mcast
# appliance: mcast serve and/or an mcast probe as services, LLDP/CDP, hardened SSH, optional Cockpit
# and Tailscale, a firewall, mDNS, multicast tuning and a watchdog.
#
# Usage: sudo ./install.sh [path/to/pi.conf]   (default: pi.conf next to this script)
#
# Settings and their defaults are in pi.conf.example. Safe to re-run.
set -euo pipefail
shopt -u patsub_replacement 2>/dev/null || true # "&" in template values stays literal

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
FILES="$HERE/files"
CONF=${1:-"$HERE/pi.conf"}
REPO="nathanbent/mcast-tools"

# --- Defaults (see pi.conf.example) -------------------------------------------
PI_HOSTNAME=""
ADMIN_USER=""
INSTALL_MCAST=yes
MCAST_SERVE=yes
MCAST_LISTEN="0.0.0.0:8650"
MCAST_TOKEN=""
MCAST_IFACE="any"
MCAST_EXTRA_ARGS=""
MCAST_PROBE_TOKEN=""
ENABLE_PROBE=no
PROBE_SERVER=""
PROBE_TOKEN=""
PROBE_ID=""
PROBE_SERVER_NAME=""
PROBE_UNICAST_PORT=5104
PROBE_EXTRA_ARGS=""
ENABLE_LLDP=yes
LLDP_CDP=yes
LLDP_INTERFACES="eth*,end*,wlan*"
LLDP_DESCRIPTION="mcast paging/monitoring appliance (Raspberry Pi)"
SSH_KEYS_ONLY=yes
ENABLE_COCKPIT=yes
ENABLE_TAILSCALE=no
ENABLE_FIREWALL=yes
ENABLE_AVAHI=yes
ENABLE_SYSCTL=yes
ENABLE_WATCHDOG=yes
ENABLE_UNATTENDED_UPGRADES=yes
JOURNAL_VOLATILE=no
INSTALL_NET_TOOLS=yes
ENABLE_MOTD=yes

if [ -f "$CONF" ]; then
    # shellcheck source=/dev/null
    . "$CONF"
elif [ $# -gt 0 ]; then
    echo "no such config file: $CONF" >&2
    exit 1
fi

ADMIN_USER=${ADMIN_USER:-${SUDO_USER:-}}
MCAST_PORT=${MCAST_LISTEN##*:}
NOTES=()

on() { [ "${1,,}" = yes ] || [ "${1,,}" = true ] || [ "$1" = 1 ]; }
step() { printf '\n\e[1m==> %s\e[0m\n' "$*"; }
note() { NOTES+=("$*"); }

# Copy a file from files/, replacing @KEY@ with values: render SRC DEST MODE [KEY VALUE]...
render() {
    local src=$1 dest=$2 mode=$3 content
    shift 3
    content=$(<"$FILES/$src")
    while [ $# -ge 2 ]; do
        content=${content//"@$1@"/"$2"}
        shift 2
    done
    install -d "$(dirname "$dest")"
    printf '%s\n' "$content" >"$dest.tmp"
    chmod "$mode" "$dest.tmp"
    mv "$dest.tmp" "$dest"
}

APT_UPDATED=no
apt_install() {
    if [ $APT_UPDATED = no ]; then
        apt-get update -q
        APT_UPDATED=yes
    fi
    DEBIAN_FRONTEND=noninteractive apt-get install -y -q --no-install-recommends "$@"
}

# --- Checks -------------------------------------------------------------------
if [ "$(id -u)" -ne 0 ]; then
    echo "run as root: sudo $0 $*" >&2
    exit 1
fi
if ! command -v apt-get >/dev/null; then
    echo "this script is for Raspberry Pi OS / Debian (apt-get not found)" >&2
    exit 1
fi
if on "$ENABLE_PROBE" && ! on "$INSTALL_MCAST"; then
    echo "ENABLE_PROBE needs INSTALL_MCAST=yes (the probe is the mcast binary)" >&2
    exit 1
fi
if on "$ENABLE_PROBE" && [ -z "$PROBE_TOKEN" ]; then
    echo "ENABLE_PROBE=yes needs PROBE_TOKEN: the server's probe token (MCAST_PROBE_TOKEN there)" >&2
    exit 1
fi
if ! grep -qi 'raspberry pi' /proc/device-tree/model 2>/dev/null; then
    echo "warning: this doesn't look like a Raspberry Pi; carrying on anyway" >&2
fi

# --- Steps --------------------------------------------------------------------
set_hostname() {
    [ -n "$PI_HOSTNAME" ] || return 0
    step "Hostname: $PI_HOSTNAME"
    hostnamectl set-hostname "$PI_HOSTNAME"
    if grep -q '^127\.0\.1\.1' /etc/hosts; then
        sed -i "s/^127\.0\.1\.1.*/127.0.1.1\t$PI_HOSTNAME/" /etc/hosts
    else
        printf '127.0.1.1\t%s\n' "$PI_HOSTNAME" >>/etc/hosts
    fi
}

install_base() {
    step "Base packages"
    local pkgs=(curl ca-certificates alsa-utils)
    on "$INSTALL_NET_TOOLS" && pkgs+=(tcpdump tshark iperf3 ethtool mtr-tiny dnsutils nmap arping socat)
    on "$ENABLE_FIREWALL" && pkgs+=(nftables)
    on "$ENABLE_LLDP" && pkgs+=(lldpd)
    on "$ENABLE_AVAHI" && pkgs+=(avahi-daemon)
    on "$ENABLE_UNATTENDED_UPGRADES" && pkgs+=(unattended-upgrades)
    # tshark asks whether non-root users may capture; the default (no) is right.
    echo 'wireshark-common wireshark-common/install-setuid boolean false' | debconf-set-selections
    apt_install "${pkgs[@]}"
}

install_mcast() {
    on "$INSTALL_MCAST" || return 0
    step "mcast"
    local arch target tmp
    arch=$(dpkg --print-architecture)
    case $arch in
        arm64) target=aarch64-unknown-linux-gnu ;;
        amd64) target=x86_64-unknown-linux-gnu ;;
        *)
            echo "no mcast release for $arch: use the 64-bit Raspberry Pi OS, or build from source" >&2
            exit 1
            ;;
    esac
    tmp=$(mktemp -d)
    local base="https://github.com/$REPO/releases/latest/download"
    curl -fsSL -o "$tmp/mcast-$target.tar.gz" "$base/mcast-$target.tar.gz"
    curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS"
    (cd "$tmp" && grep " mcast-$target.tar.gz\$" SHA256SUMS | sha256sum -c --quiet -)
    tar -xzf "$tmp/mcast-$target.tar.gz" -C "$tmp"
    install -m 0755 "$tmp/mcast" /usr/local/bin/mcast
    rm -rf "$tmp"
    /usr/local/bin/mcast --version

    if ! id mcast >/dev/null 2>&1; then
        useradd --system --home-dir /var/lib/mcast --no-create-home --shell /usr/sbin/nologin mcast
    fi
    usermod -aG audio mcast
    install -d -m 0750 -o root -g mcast /etc/mcast
}

# Print one variable from an env file an earlier run wrote (nothing if none).
saved() {
    [ -f "$1" ] || return 0
    (
        # shellcheck source=/dev/null
        . "$1"
        printf '%s' "${!2:-}"
    )
}

# Write stdin to an env file only root and the mcast group can read (it holds tokens).
write_env() {
    local dest=$1
    cat >"$dest.tmp"
    chown root:mcast "$dest.tmp"
    chmod 0640 "$dest.tmp"
    mv "$dest.tmp" "$dest"
}

setup_serve() {
    on "$INSTALL_MCAST" || return 0
    if ! on "$MCAST_SERVE"; then
        if [ -f /etc/systemd/system/mcast.service ]; then
            step "mcast serve: off"
            systemctl disable --now mcast.service || true
        fi
        return 0
    fi
    step "mcast serve"
    local env=/etc/mcast/mcast.env
    # Keep tokens from an earlier run unless pi.conf sets them.
    [ -n "$MCAST_TOKEN" ] || MCAST_TOKEN=$(saved "$env" MCAST_TOKEN)
    if [ -z "$MCAST_TOKEN" ]; then
        MCAST_TOKEN=$(head -c 18 /dev/urandom | base64 | tr '+/' '-_')
        note "mcast token (also in $env): $MCAST_TOKEN"
    fi
    [ -n "$MCAST_PROBE_TOKEN" ] || MCAST_PROBE_TOKEN=$(saved "$env" MCAST_PROBE_TOKEN)
    if [ -z "$MCAST_PROBE_TOKEN" ]; then
        MCAST_PROBE_TOKEN=$(head -c 18 /dev/urandom | base64 | tr '+/' '-_')
        note "probe token, for PROBE_TOKEN on probes (also in $env): $MCAST_PROBE_TOKEN"
    fi
    write_env "$env" <<EOF
# Read by mcast.service. Written by install.sh; edit and "systemctl restart mcast".
MCAST_LISTEN=$MCAST_LISTEN
MCAST_TOKEN=$MCAST_TOKEN
MCAST_PROBE_TOKEN=$MCAST_PROBE_TOKEN
MCAST_IFACE=$MCAST_IFACE
MCAST_EXTRA_ARGS="$MCAST_EXTRA_ARGS"
EOF

    render mcast.service /etc/systemd/system/mcast.service 0644
    systemctl daemon-reload
    systemctl enable mcast.service
    systemctl restart mcast.service
    note "mcast web GUI: http://$(hostname).local:$MCAST_PORT/   (sudo mcast update to upgrade)"
}

setup_probe() {
    on "$INSTALL_MCAST" || return 0
    if ! on "$ENABLE_PROBE"; then
        if [ -f /etc/systemd/system/mcast-probe.service ]; then
            step "mcast probe: off"
            systemctl disable --now mcast-probe.service || true
        fi
        return 0
    fi
    step "mcast probe"
    write_env /etc/mcast/probe.env <<EOF
# Read by mcast-probe.service. Written by install.sh; edit and "systemctl restart mcast-probe".
# Empty MCAST_URL: find the server by its multicast beacon. Empty MCAST_PROBE_ID: the hostname.
MCAST_URL=$PROBE_SERVER
MCAST_TOKEN=$PROBE_TOKEN
MCAST_PROBE_ID=$PROBE_ID
MCAST_SERVER_NAME=$PROBE_SERVER_NAME
PROBE_IFACE=$MCAST_IFACE
PROBE_UNICAST_PORT=$PROBE_UNICAST_PORT
PROBE_EXTRA_ARGS="$PROBE_EXTRA_ARGS"
EOF
    render mcast-probe.service /etc/systemd/system/mcast-probe.service 0644
    systemctl daemon-reload
    systemctl enable mcast-probe.service
    systemctl restart mcast-probe.service
    local id=${PROBE_ID:-$(hostname)}
    if [ -n "$PROBE_SERVER" ]; then
        note "probe '$id' reports to $PROBE_SERVER   (journalctl -u mcast-probe -f)"
    else
        note "probe '$id' finds its server by multicast beacon   (journalctl -u mcast-probe -f)"
    fi
}

setup_lldp() {
    on "$ENABLE_LLDP" || return 0
    step "LLDP"
    render lldpd.conf /etc/lldpd.d/mcast.conf 0644 \
        DESCRIPTION "${LLDP_DESCRIPTION//\"/}" INTERFACES "$LLDP_INTERFACES"
    local args=""
    on "$LLDP_CDP" && args="-c"
    if grep -q '^#\?DAEMON_ARGS=' /etc/default/lldpd 2>/dev/null; then
        sed -i "s/^#\?DAEMON_ARGS=.*/DAEMON_ARGS=\"$args\"/" /etc/default/lldpd
    else
        echo "DAEMON_ARGS=\"$args\"" >>/etc/default/lldpd
    fi
    systemctl enable lldpd.service
    systemctl restart lldpd.service
    if [ -n "$ADMIN_USER" ] && getent group _lldpd >/dev/null; then
        usermod -aG _lldpd "$ADMIN_USER" # lldpcli without sudo
    fi
    note "LLDP: lldpcli show neighbors   (which switch port am I on?)"
}

setup_ssh() {
    step "SSH"
    apt_install openssh-server
    render sshd.conf /etc/ssh/sshd_config.d/10-mcast-pi.conf 0644
    local keys_conf=/etc/ssh/sshd_config.d/11-mcast-pi-keys-only.conf home=""
    [ -n "$ADMIN_USER" ] && home=$(getent passwd "$ADMIN_USER" | cut -d: -f6)
    if on "$SSH_KEYS_ONLY"; then
        if [ -n "$home" ] && [ -s "$home/.ssh/authorized_keys" ]; then
            printf 'PasswordAuthentication no\nKbdInteractiveAuthentication no\n' >"$keys_conf"
        else
            rm -f "$keys_conf"
            note "SSH passwords left ON: no ~/.ssh/authorized_keys for '${ADMIN_USER:-?}'. Add a key and re-run."
        fi
    else
        rm -f "$keys_conf"
    fi
    sshd -t
    systemctl enable ssh.service
    systemctl reload-or-restart ssh.service
}

setup_cockpit() {
    on "$ENABLE_COCKPIT" || return 0
    step "Cockpit"
    apt_install cockpit-ws cockpit-system cockpit-bridge cockpit-networkmanager
    systemctl enable --now cockpit.socket
    note "Cockpit: https://$(hostname).local:9090/ (logs in with a system password, not an SSH key)"
}

setup_tailscale() {
    on "$ENABLE_TAILSCALE" || return 0
    step "Tailscale"
    if ! command -v tailscale >/dev/null; then
        curl -fsSL https://tailscale.com/install.sh | sh
    fi
    systemctl enable --now tailscaled.service
    tailscale status >/dev/null 2>&1 || note "Tailscale: run 'sudo tailscale up --ssh' once to log in"
}

setup_avahi() {
    on "$ENABLE_AVAHI" || return 0
    step "mDNS"
    if on "$INSTALL_MCAST" && on "$MCAST_SERVE"; then
        render mcast-http.service /etc/avahi/services/mcast.service 0644 PORT "$MCAST_PORT"
    fi
    if [ -f /usr/share/doc/avahi-daemon/examples/ssh.service ]; then
        cp /usr/share/doc/avahi-daemon/examples/ssh.service /etc/avahi/services/ssh.service
    fi
    systemctl enable avahi-daemon.service
    systemctl restart avahi-daemon.service
}

setup_sysctl() {
    on "$ENABLE_SYSCTL" || return 0
    step "Multicast tuning"
    render sysctl.conf /etc/sysctl.d/60-mcast.conf 0644
    sysctl -q --load /etc/sysctl.d/60-mcast.conf
}

setup_firewall() {
    on "$ENABLE_FIREWALL" || return 0
    step "Firewall"
    local ports=22 udp=""
    on "$INSTALL_MCAST" && on "$MCAST_SERVE" && ports+=", $MCAST_PORT"
    on "$ENABLE_COCKPIT" && ports+=", 9090"
    # A probe receives the server's unicast copies of its streams here.
    if on "$INSTALL_MCAST" && on "$ENABLE_PROBE"; then
        udp="udp dport $PROBE_UNICAST_PORT accept"
    fi
    render nftables.conf /etc/nftables.conf 0755 TCP_PORTS "$ports" PROBE_UDP "$udp"
    nft -c -f /etc/nftables.conf
    systemctl enable nftables.service
    systemctl restart nftables.service
}

setup_system() {
    if on "$ENABLE_WATCHDOG"; then
        step "Watchdog"
        render watchdog.conf /etc/systemd/system.conf.d/10-watchdog.conf 0644
        systemctl daemon-reexec
    fi

    step "Journal"
    install -d /etc/systemd/journald.conf.d
    {
        echo "[Journal]"
        echo "SystemMaxUse=100M"
        if on "$JOURNAL_VOLATILE"; then
            echo "Storage=volatile"
            echo "RuntimeMaxUse=30M"
        fi
    } >/etc/systemd/journald.conf.d/10-mcast-pi.conf
    systemctl restart systemd-journald.service

    if on "$ENABLE_UNATTENDED_UPGRADES"; then
        step "Automatic security updates"
        printf 'APT::Periodic::Update-Package-Lists "1";\nAPT::Periodic::Unattended-Upgrade "1";\n' \
            >/etc/apt/apt.conf.d/20auto-upgrades
    fi

    install -m 0755 "$FILES/mcast-pi-status" /usr/local/bin/mcast-pi-status
    if on "$ENABLE_MOTD"; then
        install -m 0755 "$FILES/motd" /etc/update-motd.d/60-mcast
    else
        rm -f /etc/update-motd.d/60-mcast
    fi
}

# --- Run ----------------------------------------------------------------------
set_hostname
install_base
setup_sysctl
install_mcast
setup_serve
setup_probe
setup_lldp
setup_ssh
setup_cockpit
setup_tailscale
setup_avahi
setup_firewall
setup_system

step "Done"
/usr/local/bin/mcast-pi-status || true
echo
for n in "${NOTES[@]}"; do echo "  * $n"; done
