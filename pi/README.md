# mcast on a Raspberry Pi

A recipe for turning a Raspberry Pi into a small, headless mcast appliance:
plug it into a switch port and it pages, monitors and announces itself to the
network, or watches multicast where it sits and reports to a server as a
[probe](../docs/probes.md).

`install.sh` sets up a fresh Raspberry Pi OS Lite install. You choose what it
installs in `pi.conf`:

| Piece | Default | What you get |
|---|---|---|
| `mcast serve` | on | Web GUI and API on `:8650` as a systemd service, under its own user, restarted if it crashes |
| `mcast probe` | off | Watches multicast here and reports to a server, as a systemd service (see [Probes](#probes)) |
| LLDP (+ CDP) | on | The switch sees the Pi's name and role, and the Pi can show you which switch, port and VLAN it's on |
| SSH | on | Root login off. Password logins off once you have a key installed |
| Cockpit | on | Web admin on `:9090`: terminal, logs, services, network, updates |
| Tailscale | off | Access from off-site with no port forwarding; `tailscale up --ssh` |
| Firewall | on | nftables: SSH, the web ports, mDNS, SAP, IGMP and multicast UDP; everything else is dropped |
| mDNS | on | `http://<hostname>.local:8650/`, and the GUI shows up in Bonjour/Avahi browsers |
| Multicast tuning | on | More IGMP memberships than the default 20, bigger socket buffers, loose reverse-path filtering |
| Watchdog | on | The hardware watchdog reboots the Pi if it hangs |
| Unattended upgrades | on | Security updates install themselves |
| Network tools | on | `tcpdump`, `tshark`, `iperf3`, `ethtool`, `mtr`, `nmap`, `arping`, `dig` |
| Login summary | on | IP addresses, LLDP neighbor and mcast status when you log in (`mcast-pi-status`) |

## Quick start

1. Flash **Raspberry Pi OS Lite (64-bit)** with Raspberry Pi Imager. In its
   settings, set the hostname, your user, and **paste your SSH public key**
   (that is what lets `install.sh` turn password logins off).
2. Boot it on wired Ethernet, then:

   ```bash
   ssh you@pager.local
   ```

   ```bash
   sudo apt-get install -y git && git clone https://github.com/nathanbent/mcast-tools.git && cd mcast-tools/pi
   ```

   ```bash
   cp pi.conf.example pi.conf && nano pi.conf
   ```

   ```bash
   sudo ./install.sh
   ```

3. It ends with a summary that includes the generated API token. Open
   `http://pager.local:8650/` and enter the token.

Re-run `sudo ./install.sh` any time you change `pi.conf`. Each step can be run again safely,
and the token is kept between runs. Upgrade mcast later with `sudo mcast update`.

The release binaries are 64-bit only, so the 32-bit OS isn't supported.

## Probes

The same script sets up a Pi as a [probe](../docs/probes.md): it watches the
multicast streams where it's plugged in and reports loss, jitter and gaps to
your `mcast serve` server, which shows them on its web GUI's Probes tab.

1. On the **server** (a Pi set up as above, or any `mcast serve`), get its
   probe token: `install.sh` prints it at the end, and it's
   `MCAST_PROBE_TOKEN` in `/etc/mcast/mcast.env`. A probe token can only
   check in, so it's safe to copy onto every probe.
2. On each **probe** Pi, in `pi.conf`:

   ```bash
   MCAST_SERVE=no          # a probe-only Pi: no web GUI, no paging
   ENABLE_PROBE=yes
   PROBE_TOKEN="the server's probe token"
   ```

   then `sudo ./install.sh`. The probe finds the server by its multicast
   beacon, and only uses one that knows its token. Where multicast doesn't
   reach the server (another subnet, say), set
   `PROBE_SERVER="http://10.0.0.5:8650"`.
3. It shows up on the server's Probes tab under its hostname
   (`PROBE_ID` to choose another name). Configure it there: which groups to
   watch, the report interval, and unicast copies for comparison.

The server raises an alert when a probe stops checking in or a stream
starts losing packets. To get them in a chat channel, set
`MCAST_ALERT_WEBHOOK` in the server's `pi.conf` to a Slack or Mattermost
incoming-webhook URL and re-run `install.sh`. See
[alerts](../docs/probes.md#alerts).

A Pi can be both a server and a probe (leave `MCAST_SERVE=yes`). The probe
runs as `mcast-probe.service`, as the `mcast` user, and restarts on its own.
Reports it can't deliver are queued in memory and sent when the server is
back, but they're lost if the probe itself restarts. The firewall opens
`PROBE_UNICAST_PORT` (UDP 5104) for the server's unicast copies. Logs:
`journalctl -u mcast-probe -f`; the login summary shows which server it found.

The probe joins groups, and listens for the server's beacon, on the
interface set by `MCAST_IFACE` in `pi.conf`, the same setting `mcast serve`
uses.

To change a probe's settings later, edit `/etc/mcast/probe.env` and
`sudo systemctl restart mcast-probe`, or change `pi.conf` and re-run
`install.sh`.

## What goes where

| File | Installed as |
|---|---|
| `files/mcast.service` | `/etc/systemd/system/mcast.service`; settings in `/etc/mcast/mcast.env` (root and the `mcast` group only, because it holds the tokens). Known probes, their configurations and the alert rules go in `/var/lib/mcast/probes.json` |
| `files/mcast-probe.service` | `/etc/systemd/system/mcast-probe.service`; settings in `/etc/mcast/probe.env` (root and the `mcast` group only) |
| `files/lldpd.conf` | `/etc/lldpd.d/mcast.conf`; CDP is switched on in `/etc/default/lldpd` |
| `files/sshd.conf` | `/etc/ssh/sshd_config.d/10-mcast-pi.conf` (plus `11-…-keys-only.conf`) |
| `files/nftables.conf` | `/etc/nftables.conf` |
| `files/sysctl.conf` | `/etc/sysctl.d/60-mcast.conf` |
| `files/mcast-http.service` | `/etc/avahi/services/mcast.service` |
| `files/watchdog.conf` | `/etc/systemd/system.conf.d/10-watchdog.conf` |
| `files/mcast-pi-status`, `files/motd` | `/usr/local/bin/mcast-pi-status`, `/etc/update-motd.d/60-mcast` |

Uploaded media is stored in `/var/lib/mcast/media`. Logs: `journalctl -u mcast -f`.

## Recommendations

### Hardware

- **Pi 4 or Pi 5, 2 GB is plenty.** mcast is a single Rust binary that uses
  little CPU, even with Opus and the voice processing turned on.
- **PoE.** A PoE+ HAT for your model, or a PoE splitter, powers the Pi from the
  switch port like a phone or speaker. That is one cable, and you can power-cycle
  it from the switch.
- **Wired Ethernet.** Multicast over Wi-Fi is unreliable: access points send it
  at the lowest data rate, and power saving delays packets. Use Wi-Fi only to
  set the Pi up.
- **Storage.** A high-endurance microSD card, or an NVMe HAT on a Pi 5. If you
  care more about card life than about keeping logs, set `JOURNAL_VOLATILE=yes`.
- **Audio, if you want local listening or a live mic.** A Pi 5 has no
  analog audio at all, and a Pi 4 has only a headphone output. Use a USB audio
  interface or a DAC/ADC HAT (any ALSA device works; `aplay -l` lists them).
- **Clock.** A Pi has no battery-backed clock, except a Pi 5 with a battery
  plugged into its RTC connector. Without one, make sure NTP reaches it, or
  logs and the stream monitor's timestamps will be wrong until it does.

### Remote access

- **SSH** is the baseline, and `mcast tui` works over it. Use keys, not
  passwords; `install.sh` turns passwords off once a key is in place.
- **Cockpit** (on by default) is the "simple remote access" choice. It's a
  lightweight web console with a real terminal, journal viewer, service
  start/stop, network and VLAN settings, and updates, and it uses about 20 MB
  of RAM when idle. It logs in with your account's *password* (Imager sets
  one), not an SSH key.
- **Tailscale** (off by default) is how to reach the Pi from off-site: no port
  forwarding or dynamic DNS, and `tailscale up --ssh` gives you SSH tied to
  your identity provider. The firewall trusts the `tailscale0` interface. A
  WireGuard tunnel to your own server does the same if you'd rather not use a
  hosted service.
- **Guacamole: probably not on the Pi itself.** Guacamole is a browser
  gateway to *other* machines' RDP, VNC and SSH. It needs Java/Tomcat, `guacd`
  and a database, and uses hundreds of MB of RAM to share a desktop this Pi
  doesn't have. It makes sense if you want this Pi to be a **jump box** into a
  site's other gear (an amplifier's web UI or a Windows paging PC, say). Then
  run the official `guacamole/guacd` and `guacamole/guacamole` containers
  with Docker on a 4 GB+ Pi, put it behind Tailscale, and don't expose it
  directly. For a browser terminal to the Pi itself, Cockpit already has one
  (or there's `ttyd` if you want only that).
- **Out-of-band console.** When the network is the problem, connect a USB-to-TTL serial
  cable to the GPIO UART (`enable_uart=1` in `/boot/firmware/config.txt`,
  then `raspi-config` → Interface → Serial). You get a login prompt without
  any network.

### LLDP and the switch

- With `lldpd` running, the switch's `show lldp neighbors` (or `show cdp
  neighbors`) lists the Pi by hostname, with a description saying what it is.
- Going the other way, `lldpcli show neighbors` (or the login summary) tells
  you which switch, port and native VLAN the Pi is plugged into. This is
  very handy when you're tracing a paging zone with a laptop and a Pi.
- LLDP-MED fast start is on, so VoIP-aware switches see the Pi quickly. If the
  switch hands out a voice VLAN over LLDP-MED, `lldpcli show neighbors
  details` shows it.
- For SNMP monitoring, add `snmpd` and run `lldpd` with `-x` (AgentX). The
  switch-side LLDP tables are usually enough, though.

### Network setup worth knowing

- **Paging VLAN.** If the speakers or phones are on their own VLAN, give the
  Pi a tagged sub-interface instead of a second cable (Raspberry Pi OS uses
  NetworkManager):

  ```bash
  sudo nmcli con add type vlan con-name paging dev eth0 id 40 ipv4.method auto
  ```

  Then set `MCAST_IFACE` to that interface's address so pages go out on it.
- **IGMP snooping.** If the switch snoops IGMP, the VLAN needs a querier
  (usually the router or the switch itself). Without one, group memberships time out
  and the monitor goes quiet after a few minutes.
- **QoS.** The API's `"tos": 184` (EF) marks paging traffic; make sure the
  switch trusts DSCP on the Pi's port.
- **PTP, for AES67 gear.** mcast uses AES67's L16/L24 payloads. Devices that
  need AES67's shared PTP clock want `linuxptp` (`ptp4l` + `phc2sys`). Check
  hardware timestamping with `ethtool -T eth0`: CM4/CM5 and the Pi 5 have it, the
  Pi 4 doesn't (software timestamping is usually fine for a monitor).

### Making it an appliance

- **Read-only root.** `raspi-config` → Performance → Overlay File System
  makes the SD card read-only, so it survives power cuts and doesn't wear out. Uploaded
  media and config changes are then lost on reboot. Turn it on once the Pi is
  set up, or put `/var/lib/mcast` on a separate writable partition.
- **A ready-made image.** The next step after this script is to build a
  flashable image with [`rpi-image-gen`](https://github.com/raspberrypi/rpi-image-gen)
  or `pi-gen`, running `install.sh` as a build stage, so a new Pi is ready as soon as it's flashed.
- **Monitoring.** `prometheus-node-exporter` is one `apt install` away if you
  already run Prometheus. `GET /api/v1/monitor` gives stream health for
  Home Assistant or similar.
