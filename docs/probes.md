# Probes

Deploy one server and a handful of small receivers (Raspberry Pis, say)
around the network. The server sends the audio; each probe watches the
multicast streams where it sits, keeps statistics, and reports them to the
server. On request the server also sends each probe a unicast copy of every
stream, so a probe can tell "multicast is broken here" apart from "the
network is broken here".

```
                 multicast audio (RTP)
   ┌────────┐ ───────────────────────────────▶ ┌────────┐
   │ server │ ── unicast copy (same packets) ─▶ │ probe  │ × 5
   │ mcast  │                                   │ mcast  │
   │ serve  │ ◀── POST /probes/{id}/checkin ─── │ probe  │
   │        │ ──── reply: the probe's config ─▶ │ run    │
   └────────┘                                   └────────┘
```

## Quick start

On the server:

```bash
mcast serve -l 0.0.0.0:8650 --token ADMIN --probe-token PROBES
```

On each probe (the ID defaults to the hostname). It finds the server by
its multicast beacon:

```bash
mcast probe run --token PROBES --id idf-3
```

or, where multicast doesn't reach (or to be explicit), name the server:

```bash
mcast probe run --server http://10.0.0.5:8650 --token PROBES --id idf-3
```

From anywhere, with the admin token:

```bash
export MCAST_URL=http://10.0.0.5:8650 MCAST_TOKEN=ADMIN
mcast probe config idf-3 --label "3rd floor IDF" --unicast --interval 5
mcast probe config --defaults --group 239.192.2.1:10000   # every probe without its own config
mcast probe list
mcast probe show idf-3
```

```
idf-3 [3rd floor IDF]  online  host pi-idf3  v0.1.1
  239.192.2.1:10000      Lobby                   50.0 pkt/s    12 lost (0.48%) jitter 0.31 ms max gap 180 ms
  Lobby vs. its unicast copy: multicast 2488 pkts, 12 lost (0.48%) | unicast 2500 pkts, 0 lost (0.00%) -> LOSS ON MULTICAST ONLY: ...
```

On a Raspberry Pi, `pi/install.sh` sets this up as a systemd service: see
[pi/README.md](../pi/README.md#probes).

## What each side does

### Server (`mcast serve`)

- Sends audio, as before (pages from the web GUI, TUI or API).
- Keeps a registry of probes: who checked in when, from which host and
  version, and what configuration it runs.
- Configures probes: each has its own configuration or follows the
  defaults. Probes can be configured before they're deployed. Known probes
  and their configurations are saved in `~/.local/share/mcast/probes.json`
  (`--probe-state FILE`), so they survive a restart.
- Receives a heartbeat: every check-in is one. A probe that misses three
  report intervals (plus 5 s) shows as offline and stops getting unicast copies.
- Sends unicast copies: with `unicast` on in a probe's configuration, every
  sender job copies each RTP packet to that probe (same SSRC, sequence
  numbers and timestamps, same DSCP marking), from a separate socket so the
  multicast stream is undisturbed.
- Receives stat uploads: the reports in each check-in, deduplicated, kept for
  the last 360 intervals per probe, plus running totals per stream.
- Compares the two paths per stream (matched by SSRC) and gives a verdict.

### Probe (`mcast probe run`)

- Receives the streams: every SAP-announced stream and/or listed groups,
  using the same monitor as `mcast monitor` (rate, loss, late packets,
  jitter, largest gap, TTL, DSCP, audio level, RTCP).
- Has a unicast relationship with the server: it opens the connection, so
  it needs no open port for control and works behind NAT or a firewall.
  Only the unicast test needs an inbound UDP port (`--unicast-port`, 5104).
- Is configured by the server: the reply to each check-in carries its
  configuration, and the probe restarts its monitor when that changes.
- Does the unicast test: it listens on the unicast port and measures the
  copies like any other stream.
- Tracks stats and uploads them: one report per interval. Reports the server
  hasn't acknowledged stay queued (`--max-queue`, 2000 by default, about
  5.5 hours at 10 s) and are delivered once it's reachable again, so an outage
  of the control path doesn't lose the record of how multicast did meanwhile.

### Verdicts

| Verdict | Multicast | Unicast copy | Points at |
|---|---|---|---|
| `ok` | clean | clean | |
| `multicast_loss` | loss | clean | the multicast path: IGMP snooping, a missing querier, PIM, a Wi-Fi hop's multicast rate |
| `both_loss` | loss | loss | the network as a whole (congestion, a bad link) or the sender |
| `unicast_loss` | clean | loss | unusual; the copy is the extra traffic (a policer on unicast?) |
| `no_multicast` | nothing | arriving | the probe isn't getting the group: not joined, snooping without a querier, TTL, routing |
| `idle` | nothing | nothing | the stream is paused |

Each is for one report interval; the totals (`mcast probe show`) count how
many intervals had loss, and the largest gap and worst jitter seen.

## Discovery

A server listening on anything but loopback announces itself every 5 s on
`239.255.86.50:8651` (`--beacon-group`, `--beacon-ttl` 16, `--no-beacon`
to stop) with a small JSON message:

```json
{"service": "mcast-probe-server", "v": 1, "name": "pager", "port": 8650, "url": null}
```

A probe started without `--server` listens there and uses
`http://<the beacon's source address>:<port>`, or `url` when the server sets
one with `--beacon-url` (for a TLS proxy, say). `mcast probe discover` lists
the servers heard.

Before it sends its token to a server it found, the probe checks the server
already knows it: it sends a random nonce to `GET /api/v1/probe-hello`
(no token needed) and the server answers with HMAC-SHA256 of the nonce
under each token it accepts. A server that can't prove it knows the probe's
token is ignored (and asked again a minute later). So with several servers
on one network (a lab and production, say), each probe reports to the one
whose token it was given; `--server-name` narrows it further by the server's
hostname.

The probe stays with its server while that keeps announcing itself, follows
it to a new address (the same name from another address, after a DHCP
change), and moves to another server that passes the check only when its
own has been silent for 20 s. If beacons stop but the server still answers,
check-ins carry on.

What the check is not: protection against an attacker on the network path.
Everything is plain HTTP, so anyone who can see a check-in can read the
token, and an attacker who can also reach the server could relay the
challenge. It stops probes reporting to the wrong server by mistake and
casual spoofing; for more, pin `--server` to an HTTPS URL behind a TLS
proxy and turn the beacon off.

## Protocol

Plain HTTP/JSON on the existing API, so it inherits its token auth and can
sit behind the same TLS reverse proxy. See [api.md](api.md#probes) for the
routes and fields. In short:

1. Every `report_interval_s` (10 s by default) the probe POSTs
   `/api/v1/probes/{id}/checkin` with its version, hostname, a random `boot`
   ID for this run, the configuration it's running, where it wants unicast
   copies, and up to 30 unacknowledged reports (oldest first), each numbered
   `seq` from 1 for this boot.
2. The server stores the reports it doesn't have yet (by `boot` and `seq`) and
   replies with the configuration the probe should run and `acked_seq`,
   the last report it holds. The probe forgets those.
3. If more reports are queued, the probe checks in again at once; otherwise
   at its next report. When the server can't be reached it retries with
   backoff (2 s, doubling, up to the report interval).

The unicast address is the one the probe's host uses to reach the server
(found by routing, no packet is sent), or `--advertise IP`.

Security: `--probe-token` gives probes a token that only opens the check-in
route, so the token copied onto every Pi can't be used to page. A probe can
claim any ID and send unicast copies to any address it names; that's
acceptable with the token kept to your own probes, and the copies stop
once a probe goes offline.

## Roadmap

Done (this change):

- [x] Probe registry, check-in/heartbeat, offline detection, `probe_updated`
      and `probe_removed` events
- [x] Server-side configuration per probe and defaults, saved across restarts
- [x] Stat upload with acknowledgement, retry and an offline queue
- [x] Unicast copies of every sender job, and the multicast vs. unicast verdict
- [x] `mcast probe run | list | show | config | remove`, probe-only token
- [x] Discovery: the server's multicast beacon, `/probe-hello` token check,
      `mcast probe discover`
- [x] TUI Probes tab
- [x] Raspberry Pi packaging: `mcast-probe.service`, and `pi/install.sh`
      with `ENABLE_PROBE=yes` (see [pi/README.md](../pi/README.md#probes))

Next:

- [x] Web GUI Probes view (`#probes`): online state, verdicts, loss timeline,
      configuration, adding probes ahead of deployment
- [ ] Alerts: a probe offline, `multicast_loss` or `no_multicast` for N
      intervals, as events (and optionally a webhook)
- [ ] Persist report history (not only configurations), e.g. a JSON-lines file
      per probe with rotation, for "how was multicast overnight?"
- [ ] Unicast copies of only some streams (per probe list of groups), and
      a bandwidth cap: a copy per probe per stream adds up for L24
- [ ] Probe-side extras the server could ask for: play to a local output so
      someone on site can listen, record a short WAV on loss, run `mcast
      ping`-style round-trip tests
- [ ] Measure one-way delay of both paths (RTCP SR to the probe, or
      server timestamps) and clock offset from `server_time_ms`
- [ ] Mutual TLS or per-probe tokens if probes go beyond a trusted LAN
