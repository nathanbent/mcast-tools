#!/usr/bin/env bash
# install.sh - turn a fresh Raspberry Pi OS Lite (64-bit) install into an mcast
# appliance: mcast serve as a service, optionally the intercom panel, LLDP/CDP,
# hardened SSH, optional Cockpit and Tailscale, a firewall, mDNS, multicast
# tuning and a watchdog.
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
PI_TIMEZONE=""
ADMIN_USER=""
INSTALL_MCAST=yes
MCAST_LISTEN="0.0.0.0:8650"
MCAST_TOKEN=""
MCAST_IFACE="any"
MCAST_EXTRA_ARGS=""
ENABLE_INTERCOM=no
INTERCOM_LISTEN="0.0.0.0:8651"
INTERCOM_TOKEN=""
INTERCOM_HTTPS=yes
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
INTERCOM_PORT=${INTERCOM_LISTEN##*:}
# Set once the intercom service is installed and running.
INTERCOM_ON=no
INTERCOM_SCHEME=http
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

# How to reach a server listening on LISTEN (host:port) from this Pi.
local_url() {
    local host=${1%:*} port=${1##*:}
    case $host in
        0.0.0.0 | "") host=127.0.0.1 ;;
        "[::]") host="[::1]" ;;
    esac
    echo "http://$host:$port"
}

# A value from an env file written by an earlier run: env_value FILE KEY
env_value() {
    [ -f "$1" ] || return 0
    # shellcheck source=/dev/null
    (. "$1" && printf '%s\n' "${!2:-}")
}

new_token() {
    head -c 18 /dev/urandom | base64 | tr '+/' '-_'
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

set_timezone() {
    [ -n "$PI_TIMEZONE" ] || return 0
    step "Time zone: $PI_TIMEZONE"
    timedatectl set-timezone "$PI_TIMEZONE"
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
    # The intercom's buttons and door contacts read /dev/gpiochip* (group gpio
    # on Raspberry Pi OS).
    if getent group gpio >/dev/null; then
        usermod -aG gpio mcast
    fi

    install -d -m 0750 -o root -g mcast /etc/mcast
    local env=/etc/mcast/mcast.env
    # Keep a token from an earlier run unless pi.conf sets one.
    [ -n "$MCAST_TOKEN" ] || MCAST_TOKEN=$(env_value "$env" MCAST_TOKEN)
    if [ -z "$MCAST_TOKEN" ]; then
        MCAST_TOKEN=$(new_token)
        note "mcast token (also in $env): $MCAST_TOKEN"
    fi
    umask 027
    cat >"$env" <<EOF
# Read by mcast.service. Written by install.sh; edit and "systemctl restart mcast".
MCAST_LISTEN=$MCAST_LISTEN
MCAST_TOKEN=$MCAST_TOKEN
MCAST_IFACE=$MCAST_IFACE
MCAST_EXTRA_ARGS="$MCAST_EXTRA_ARGS"
EOF
    umask 022
    chown root:mcast "$env"
    chmod 0640 "$env"

    render mcast.service /etc/systemd/system/mcast.service 0644
    systemctl daemon-reload
    systemctl enable mcast.service
    systemctl restart mcast.service
    note "mcast web GUI: http://$(hostname).local:$MCAST_PORT/   (sudo mcast update to upgrade)"
}

install_intercom() {
    local unit=/etc/systemd/system/mcast-intercom.service
    if ! on "$ENABLE_INTERCOM"; then
        # Switched off after being on: stop it, but keep its config and token.
        if [ -f "$unit" ]; then
            step "Intercom panel: off"
            systemctl disable --now mcast-intercom.service || true
            rm -f "$unit"
            systemctl daemon-reload
        fi
        return 0
    fi
    step "Intercom panel"
    if [ ! -x /usr/local/bin/mcast ] || [ ! -f /etc/mcast/mcast.env ]; then
        echo "the intercom panel plays through mcast serve: set INSTALL_MCAST=yes too" >&2
        exit 1
    fi
    if ! /usr/local/bin/mcast intercom --help >/dev/null 2>&1; then
        note "Intercom panel NOT installed: $(/usr/local/bin/mcast --version) is too old (it needs 0.4.0 or later). Run 'sudo mcast update', then re-run install.sh."
        return 0
    fi

    local env=/etc/mcast/intercom.env mcast_listen
    [ -n "$INTERCOM_TOKEN" ] || INTERCOM_TOKEN=$(env_value "$env" MCAST_INTERCOM_TOKEN)
    if [ -z "$INTERCOM_TOKEN" ]; then
        INTERCOM_TOKEN=$(new_token)
        note "intercom panel token (also in $env): $INTERCOM_TOKEN"
    fi
    mcast_listen=$(env_value /etc/mcast/mcast.env MCAST_LISTEN)
    # Browsers only allow push to talk's microphone on HTTPS pages.
    local tls_line="MCAST_INTERCOM_NO_TLS=true"
    if on "$INTERCOM_HTTPS"; then
        if /usr/local/bin/mcast intercom serve --help 2>/dev/null | grep -q -- --tls-self-signed; then
            tls_line="MCAST_INTERCOM_TLS_SELF_SIGNED=true"
            INTERCOM_SCHEME=https
        else
            note "Intercom panel over plain HTTP: $(/usr/local/bin/mcast --version) can't serve HTTPS (it needs 0.5.0 or later; push to talk needs HTTPS). Run 'sudo mcast update', then re-run install.sh."
        fi
    fi
    umask 027
    cat >"$env" <<EOF
# Read by mcast-intercom.service. Written by install.sh; edit and
# "systemctl restart mcast-intercom". Its token for mcast serve is
# MCAST_TOKEN in mcast.env.
MCAST_INTERCOM_LISTEN=$INTERCOM_LISTEN
MCAST_INTERCOM_TOKEN=$INTERCOM_TOKEN
MCAST_URL=$(local_url "${mcast_listen:-$MCAST_LISTEN}")
MCAST_INTERCOM=/var/lib/mcast/intercom.json
# HTTPS with a self-signed certificate (kept in /var/lib/mcast), so push to
# talk can use the browser's microphone; or, with INTERCOM_HTTPS=no, plain
# HTTP. For your own certificate, set MCAST_INTERCOM_TLS_CERT and
# MCAST_INTERCOM_TLS_KEY.
$tls_line
EOF
    umask 022
    chown root:mcast "$env"
    chmod 0640 "$env"

    render mcast-intercom.service "$unit" 0644
    systemctl daemon-reload
    systemctl enable mcast-intercom.service
    systemctl restart mcast-intercom.service
    INTERCOM_ON=yes
    note "Intercom panel: $INTERCOM_SCHEME://$(hostname).local:$INTERCOM_PORT/   (its config: /var/lib/mcast/intercom.json)"
    [ $INTERCOM_SCHEME = https ] && note "  Its certificate is self-signed: browsers warn about it once; accept it to go on."
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
    if on "$INSTALL_MCAST"; then
        render mcast-http.service /etc/avahi/services/mcast.service 0644 \
            NAME "mcast on %h" PORT "$MCAST_PORT" TYPE _http._tcp
    fi
    if [ $INTERCOM_ON = yes ]; then
        render mcast-http.service /etc/avahi/services/mcast-intercom.service 0644 \
            NAME "Intercom on %h" PORT "$INTERCOM_PORT" TYPE "_$INTERCOM_SCHEME._tcp"
    else
        rm -f /etc/avahi/services/mcast-intercom.service
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
    local ports=22
    on "$INSTALL_MCAST" && ports+=", $MCAST_PORT"
    [ $INTERCOM_ON = yes ] && ports+=", $INTERCOM_PORT"
    on "$ENABLE_COCKPIT" && ports+=", 9090"
    render nftables.conf /etc/nftables.conf 0755 TCP_PORTS "$ports"
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
set_timezone
install_base
setup_sysctl
install_mcast
install_intercom
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
