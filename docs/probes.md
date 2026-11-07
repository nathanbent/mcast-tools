# Probes

A probe is a small receiver, such as a Raspberry Pi, that you leave somewhere
on the network: in a wiring closet, on a paging zone's VLAN, behind a Wi-Fi
access point. It watches the multicast streams where it sits and reports
loss, jitter and gaps to your `mcast serve` server. The server shows how
multicast is doing at every probe, and raises an alert when a probe goes
quiet or a stream starts losing packets.

The server can also send each probe a **unicast copy** of every stream it
pages. The probe measures both, so it can tell "multicast is broken here"
(IGMP snooping, a missing querier, PIM, Wi-Fi) apart from "the network is
broken here" (a bad link, congestion).

```
                 multicast audio (RTP)
   ┌────────┐ ───────────────────────────────▶ ┌────────┐
   │ server │ ── unicast copy (same packets) ─▶ │ probe  │ × 5
   │ mcast  │                                   │ mcast  │
   │ serve  │ ◀── POST /probes/{id}/checkin ─── │ probe  │
   │        │ ──── reply: the probe's config ─▶ │ run    │
   └────────┘                                   └────────┘
```

Contents: [Quick start](#quick-start) · [Seeing the results](#seeing-the-results) ·
[Configuring probes](#configuring-probes) · [Unicast comparison](#unicast-comparison) ·
[Alerts](#alerts) · [Discovery](#discovery) · [How it works](#how-it-works) ·
[Security](#security) · [Troubleshooting](#troubleshooting) · [Limits and roadmap](#limits-and-roadmap)

## Quick start

On the server, give it an admin token and a probe token. The probe token
can only check in, so it's the one to copy onto every probe:

```bash
mcast serve -l 0.0.0.0:8650 --token ADMIN --probe-token PROBES
```

On each probe:

```bash
mcast probe run --token PROBES
```

It finds the server by the server's multicast beacon and checks in under its
hostname (`--id NAME` to choose another). Where multicast from the server
doesn't reach the probe, for example on another subnet, name the server:

```bash
mcast probe run --server http://10.0.0.5:8650 --token PROBES
```

Other `mcast probe run` options:
- `--iface IP`: the interface to join groups and hear the beacon on.
- `--sap-group ADDR:PORT`: the SAP groups to listen on, if your senders
  announce somewhere other than the usual ones. The probe uses its own,
  not the server's.
- `--unicast-port`, `--advertise`: see [Unicast comparison](#unicast-comparison).
- `--max-queue`: how many reports to keep while the server is unreachable.
- `--verbose`: print every report.

`--server`, `--token`, `--id` and `--server-name` can also come from
`MCAST_URL`, `MCAST_TOKEN`, `MCAST_PROBE_ID` and `MCAST_SERVER_NAME`. An
empty value counts as not set.

On a Raspberry Pi, `pi/install.sh` sets the probe up as a systemd service
(`ENABLE_PROBE=yes`); see [pi/README.md](../pi/README.md#probes).

Within a few seconds the probe appears on the server's **Probes** page
(`http://10.0.0.5:8650/#probes`). By default it watches every stream
announced with SAP and reports every 10 s.

## Seeing the results

**Web GUI**: the Probes tab (`/#probes`). It has a card per probe with its
state, the streams it sees, the unicast comparison and a loss timeline, plus
the alerts. The tab's red count is the number of active alerts.

**Terminal**: `mcast tui` has a Probes tab (7). It shows active alerts at
the top, a row per probe, and the selected probe's streams, verdicts and
timeline.

**Command line**, from anywhere that can reach the server, with the admin
token (don't start a probe in this shell: `mcast probe run` reads the same
variables, so it would skip discovery and use the admin token):

```bash
export MCAST_URL=http://10.0.0.5:8650 MCAST_TOKEN=ADMIN
mcast probe list            # every probe, with its latest report
mcast probe show idf-3      # one probe, with totals since it was first seen
mcast probe alerts --all    # active alerts, then resolved ones
```

```
idf-3 [3rd floor IDF]  online  host pi-idf3  v0.2.0
  239.192.2.1:10000      Lobby                   50.0 pkt/s    12 lost (0.48%) jitter 0.31 ms max gap 180 ms
    Lobby vs. its unicast copy: multicast 488 pkts, 12 lost (0.48%) | unicast 500 pkts, 0 lost (0.00%) -> LOSS ON MULTICAST ONLY: ...
```

Each report covers one interval: packets, lost and late packets, loss
percentage, jitter (RFC 3550), the largest gap between packets, TTL, DSCP
and audio level, per sender per group. The totals add up every report since
the server first saw the probe (since the server started, or since the probe
was forgotten and came back): packets, loss, how many intervals had loss,
the largest gap and the worst jitter.

**API and events**: `GET /api/v1/probes`, `/probes/{id}/reports`,
`/alerts`, and the `probe_updated`, `alert_raised` and `alert_resolved`
server-sent events; see [api.md](api.md#probes).

## Configuring probes

Probes are configured on the server, not on the probe. Each probe either
follows the **defaults** or has a configuration of its own. The probe picks
up a change at its next check-in.

| Setting | Default | |
|---|---|---|
| `label` | none | Shown in listings and alerts, e.g. "3rd floor IDF" |
| `sap` | on | Watch every stream announced with SAP |
| `groups` | none | Also watch these multicast groups (for streams that aren't announced) |
| `report_interval_s` | 10 | Seconds per report. This is also how often it checks in, from 1 to 3600 |
| `unicast` | off | Have the server send this probe unicast copies of its pages ([below](#unicast-comparison)) |

```bash
mcast probe config idf-3 --label "3rd floor IDF" --unicast --interval 5
mcast probe config idf-3 --group 239.192.2.1:10000 --group 239.192.2.2:10000 --no-sap
mcast probe config idf-3 --reset                          # back to the defaults
mcast probe config --defaults --group 239.192.2.1:10000   # for every probe without its own
```

The same settings are in the web GUI: Configure on a probe's card, and
Defaults for the defaults. In the TUI, on the Probes tab, `u`, `s`, `l`, `g`
and `i` change the selected probe, and `D` puts it back on the defaults. The
defaults themselves and the alert rules can't be changed from the TUI.

You can configure a probe before it's deployed: `mcast probe config NAME
...`, or "Add probe" in the web GUI. It gets that configuration when it
first checks in. A probe shows "config not applied yet" until it confirms it
runs the configuration it was given.

Known probes and their configurations, the defaults and the alert rules are
kept in `probes.json`, so they survive a server restart. It's in
`$XDG_DATA_HOME/mcast/` (normally `~/.local/share/mcast/`), or at
`/var/lib/mcast/probes.json` on a Pi set up by `pi/install.sh`; `mcast serve
--probe-state FILE` puts it elsewhere. Reports and totals are kept in memory
only.

## Unicast comparison

Multicast loss at a probe can be the network (a bad link, congestion, the
sender) or multicast itself (a switch snooping IGMP without a querier, PIM,
a Wi-Fi access point sending multicast at its lowest rate). To tell them
apart, turn on `unicast` for a probe:

```bash
mcast probe config idf-3 --unicast
```

From then on, every page the server sends is also sent to that probe by
unicast: identical packets, with the same SSRC, sequence numbers,
timestamps and DSCP marking. They're sent from a separate socket, so the
multicast stream is undisturbed. The probe receives them on UDP port 5104
(`mcast probe run --unicast-port`). It measures both copies, and the server
compares them stream by stream:

| Verdict | Multicast | Unicast copy | Points at |
|---|---|---|---|
| `ok` | clean | clean | |
| `multicast_loss` | loss | clean | the multicast path: IGMP snooping, a missing querier, PIM, a Wi-Fi hop's multicast rate |
| `both_loss` | loss | loss | the network as a whole (congestion, a bad link) or the sender |
| `unicast_loss` | clean | loss | unusual: the copy is the extra traffic (a policer on unicast?) |
| `no_multicast` | nothing | arriving | the probe isn't getting the group: not joined, snooping without a querier, TTL, routing |
| `idle` | nothing | nothing | the stream is paused |

A verdict covers one report interval. Things to know:

- Only pages started by this server (from the web GUI, TUI or API) are
  copied. Streams sent by other senders, or by `mcast audio send` on the
  command line, are watched but not compared.
- The copies go to the address the probe's host uses to reach the server,
  which it works out from its routing table. Behind NAT, or to use another
  interface, give it with `mcast probe run --advertise IP`.
- Each copy is a stream's full bandwidth again, per probe. That's nothing for
  G.711 or Opus, but noticeable for L24 to many probes.
- A probe that goes offline stops getting copies.

## Alerts

The server raises an alert when:

| Alert | When |
|---|---|
| `probe_offline` | A probe misses three check-ins in a row, plus 5 s (35 s at the default interval). Dated from its last check-in |
| `multicast_loss` | A multicast stream (one sender on one group) loses at least `loss_percent` of its packets in `raise_after` report intervals in a row. If the stream's unicast copy arrived too, the message says whether the copy was clean (look at the multicast path) or lossy too (look at the network or the sender) |
| `no_multicast` | For `raise_after` intervals in a row, a probe gets a stream's unicast copy but not the stream itself |

An alert is resolved after `clear_after` clean intervals in a row, when a
stream stops (its intervals count as clean), when an offline probe checks in
again, when the probe is forgotten, or when its kind is turned off. One bad
interval doesn't raise anything, and a stream that flaps doesn't raise a new
alert every few seconds.

Alerts are dated by the reports, not by when the server heard about them. A
probe that was cut off from the server delivers its queued reports when it
gets back, and loss it saw meanwhile is raised (and maybe resolved) with the
right times.

**Rules** apply to every probe:

| Rule | Default | |
|---|---|---|
| `offline` | on | Alert when a probe stops checking in |
| `multicast_loss` | on | Alert on multicast loss |
| `loss_percent` | 1 | Loss in one report interval that counts as bad, in percent (more than 0, at most 100) |
| `no_multicast` | on | Alert when the unicast copy arrives but the stream doesn't |
| `raise_after` | 2 | Bad intervals in a row before an alert is raised (1 to 100) |
| `clear_after` | 3 | Clean intervals in a row before it's resolved (1 to 100) |

```bash
mcast probe alert-rules                                  # show them
mcast probe alert-rules --loss-percent 0.5 --raise-after 3
mcast probe alert-rules --offline off
```

They're also under "Alert rules" on the web GUI's Probes tab.

**Where alerts show up**:

- the web GUI's Probes tab, with a count on the tab
- `mcast tui`'s Probes tab, with a count on the tab and a status line when
  one is raised or resolved
- `mcast probe alerts`
- `GET /api/v1/alerts`, and the `alert_raised` and `alert_resolved` events
- a webhook, if the server has one

**Webhook**: `mcast serve --alert-webhook URL` (or `MCAST_ALERT_WEBHOOK`)
POSTs every alert that's raised or resolved:

```json
{"text": "ALERT: idf-3 (3rd floor IDF): 2.4% multicast loss on Lobby; its unicast copy is clean, so look at the multicast path (IGMP snooping/querier, PIM, Wi-Fi)",
 "event": "alert_raised",
 "alert": {"id": 7, "kind": "multicast_loss", "probe": "idf-3", "label": "3rd floor IDF", "stream": "Lobby",
           "group": "239.192.2.1:10000", "message": "...", "raised_ms": 1794026269854, "resolved_ms": null,
           "worst_loss_percent": 2.4, "unicast_clean": true}}
```

Slack, Mattermost and Rocket.Chat incoming webhooks show `text` as a
message, so the URL they give you works as it is. For anything else, read
`event` and `alert`. Posts are sent in order from a background thread; each
is tried three times (2 s, then 4 s apart) and then dropped with a line in
the server's log.

The server keeps active alerts and the last 200 resolved ones in memory. They
start empty after a restart, and a probe that's already offline then isn't
alerted until it has checked in once.

## Discovery

A server listening on anything but loopback announces itself every 5 s on
`239.255.86.50:8651` with a small JSON message:

```json
{"service": "mcast-probe-server", "v": 1, "name": "pager", "port": 8650, "url": null}
```

A probe started without `--server` listens for it and reports to
`http://<the beacon's source address>:<port>`, or to `url` when the server
sets one with `--beacon-url` (for a TLS proxy, say). `mcast probe discover`
lists the servers heard.

**Picking the right server.** Before it sends its token to a server it
found, the probe checks the server already knows it. It sends a random
challenge to `GET /api/v1/probe-hello`, which needs no token, and the server
answers with an HMAC-SHA256 of it under each token it accepts. A server that
can't prove it knows the probe's token is ignored, and asked again a minute
later. So with several servers on one network (a lab and production, say),
each probe reports to the one whose token it was given. `--server-name NAME`
narrows it further, to servers whose hostname contains NAME.

**Staying with it.** The probe keeps its server while that keeps announcing
itself. Once its server has been silent for 20 s, it moves on. It prefers
the same server name heard from a new address (after a DHCP change, say),
and otherwise picks another server that passes the check. If the beacons
stop and nothing else is heard, it keeps checking in with the server it has.

**Server settings**:
- `--no-beacon` turns the beacon off.
- `--beacon-group ADDR:PORT` changes the group. Probes need the same one: `mcast probe run --beacon-group`.
- `--beacon-ttl N` (16 by default) sets how many routers the beacon may cross.
- `--beacon-url URL` sets the URL probes use instead of the beacon's source address and port.

## How it works

Everything goes over the server's existing HTTP/JSON API, so it uses the
same token auth and can sit behind the same TLS reverse proxy. The probe
always opens the connection, so it needs no open port for control and works
behind NAT or a firewall. The only inbound port it uses is UDP 5104, for the
unicast copies.

1. Every `report_interval_s` the probe POSTs `/api/v1/probes/{id}/checkin`
   with:
   - its version and hostname
   - a random `boot` ID for this run
   - the configuration it's running
   - where it wants unicast copies
   - up to 30 reports the server hasn't acknowledged yet, oldest first. Each
     report is numbered `seq`, counting from 1 for this boot.
2. The server stores the reports it doesn't already have (by `boot` and
   `seq`, so retries never count twice). It replies with the configuration
   the probe should run and `acked_seq`, the last report it holds. The probe
   forgets those.
3. If more reports are queued, the probe checks in again at once, and also
   straight after applying a new configuration. Otherwise it checks in at its
   next report. While the server can't be reached it retries after 2 s,
   doubling up to 64 s or the report interval, whichever is shorter.

The check-in is the heartbeat: a probe that misses three in a row, plus 5 s,
shows as offline.

What's kept where:

| | Where | For how long |
|---|---|---|
| Reports not yet delivered | the probe, in memory | up to `--max-queue` (2000, about 5.5 h at 10 s), until the probe restarts |
| Reports | the server, in memory | the last 360 per probe (an hour at 10 s), until the server restarts |
| Totals per stream | the server, in memory | since the server started |
| Probes, configurations, alert rules | the server, `probes.json` | until changed |
| Alerts | the server, in memory | active ones, and the last 200 resolved |

The full protocol, with every field, is in [api.md](api.md#probes).

## Security

- **Two tokens.** `--token` is the admin token: it can page, configure and
  read everything. `--probe-token` only opens `POST
  /api/v1/probes/{id}/checkin`, so the token copied onto every probe can't be
  used to page. Probes may use the admin token too, but shouldn't. The probe
  token only means something alongside an admin token: without `--token`
  every route is open, so `mcast serve` refuses `--probe-token` alone. That
  includes a server listening on loopback behind a proxy.
- **Plain HTTP.** Anyone who can see the traffic can read a token. On an
  untrusted network, put the server behind a TLS reverse proxy, give probes
  `--server https://...`, and turn the beacon off (`--no-beacon`).
- **The discovery check** keeps a probe's token away from a server that
  doesn't already know it, and stops probes reporting to the wrong server by
  mistake. It doesn't stop an attacker on the network path. Such an attacker
  can read tokens from plain HTTP anyway, and could also relay the challenge
  to the real server.
- **What a probe token allows.** Whoever has it can check in under any ID,
  which adds that probe and saves it in `probes.json`. A probe can also ask
  for unicast copies at any address it names. Copies go only to probes whose
  configuration has `unicast` on, and stop once the probe goes offline. Turn
  `unicast` on for chosen probes, not in the defaults: in the defaults it
  applies to new IDs too. Then anyone with the probe token could have every
  page copied to an address of their choosing.

## Troubleshooting

**A probe never appears on the server.** Check its log (`journalctl -u
mcast-probe -f` on a Pi, or run `mcast probe run` in a terminal).
- "looking for a server" that never finds one: the beacon isn't reaching it.
  Run `mcast probe discover` on the probe to check. The server must listen on
  a non-loopback address and not have `--no-beacon`, the probe must be in
  multicast range (raise `--beacon-ttl` across routers), and
  `--beacon-group` must match on both sides. Or skip discovery with `--server URL`.
- "ignoring the server at ...: it doesn't know this probe's token": the
  probe's `--token` isn't the server's `--probe-token` (or `--token`).
- "can't reach the server": a firewall, the wrong URL, or the server isn't running.
- "the server said: missing or wrong token": the token is wrong.

**"Config not applied yet" doesn't go away.** The probe hasn't checked in
since the change (it's offline, or on a long report interval), or it
couldn't start monitoring as asked. In that case it reports that it runs
no configuration until it can. Its log says why, and it tries again at every
check-in.

**An error under a probe's streams, like "0.0.0.0:5104: Address already in
use".** The probe couldn't listen on a group or its unicast port; another
program has the port. Pick another with `mcast probe run --unicast-port`.

**No streams in a probe's reports.** It isn't receiving any multicast.
- With `sap` on, are streams announced? Run `mcast audio discover` on the
  probe.
- If you listed `groups`, are they being sent?
- On a switch that snoops IGMP, the VLAN needs a querier, or the probe's
  membership times out after a few minutes.

**`no_multicast` on every stream.** The unicast copies arrive but the
groups don't: the classic sign of IGMP snooping without a querier, a router
not forwarding multicast to the probe's subnet (PIM), or a TTL too low for
the hops to the probe (`ttl` in the send request).

**Unicast comparison never shows.** Turn on `unicast` for the probe, and
check the probe's firewall lets UDP 5104 in (`pi/install.sh` opens it).
Only pages this server sends are copied. A probe behind NAT needs
`--advertise IP` and a port forward.

**An alert came late, or resolved itself at once.** A probe catching up
after being cut off delivers old reports, and alerts are dated by them. The
web GUI shows when each alert was raised and resolved.

## Limits and roadmap

Today:

- Reports, totals and alerts live in the server's memory; a restart starts
  them afresh.
- Every page is copied to every probe with `unicast` on: there's no choosing
  streams per probe, and no bandwidth cap.
- No one-way delay or clock comparison between the two paths.
- One token for all probes; no per-probe tokens or mutual TLS.

Ideas for next:

- Keep report history on disk (JSON lines per probe, with rotation), for
  "how was multicast overnight?"
- Unicast copies of chosen groups per probe, with a bandwidth cap
- One-way delay of both paths (RTCP sender reports, or server timestamps),
  and the probe's clock offset from `server_time_ms`
- On request, have a probe play a stream on a local output so someone on
  site can listen, or record a short WAV when loss starts
- More alert outputs: email, or a Microsoft Teams card
- Per-probe tokens, or mutual TLS, for probes beyond a trusted LAN
