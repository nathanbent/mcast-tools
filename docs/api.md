# mcast API

`mcast serve` exposes sending, receiving, discovery and monitoring over
HTTP/JSON. Every route is under `/api/v1`.

```bash
mcast serve [--listen 127.0.0.1:8650] [--token TOKEN] [--media-dir DIR]
            [--max-upload-mb 200] [--iface IFACE] [--sap-group ADDR:PORT]...
            [--watch ADDR:PORT]... [--no-monitor]
            [--probe-token TOKEN] [--probe-state FILE]
            [--no-beacon] [--beacon-group ADDR:PORT] [--beacon-url URL] [--beacon-ttl 16]
            [--alert-webhook URL]
```

The probe, beacon and alert options are explained in [probes.md](probes.md).
The secrets can come from the environment instead of the command line:
`MCAST_TOKEN`, `MCAST_PROBE_TOKEN` and `MCAST_ALERT_WEBHOOK` (a chat
webhook URL is a secret too).

Ctrl-C or SIGTERM stops every job (withdrawing their SAP announcements) before exiting.

## Authentication

With `--token` (or `MCAST_TOKEN`), every request needs
`Authorization: Bearer TOKEN`, except `GET /probe-hello`, which needs none.
Browsers' `EventSource` can't set headers, so `?access_token=TOKEN` is
accepted too. Without `--token` every route is open, so the server won't
start on a non-loopback address without one.

`--probe-token` (or `MCAST_PROBE_TOKEN`) is a second token that only opens
`POST /probes/{id}/checkin`, for the probes. It needs `--token` as well.

The server speaks plain HTTP: beyond a trusted LAN, put it behind a TLS
reverse proxy.

## Errors

Errors are JSON: `{"error": "message"}`.

| Status | When |
|---|---|
| 401 | Missing or wrong token |
| 404 | No such job, media file or route |
| 409 | Sending to a group another job is already sending to (adds `"job": ID`, the job sending there); text to a channel that has ended |
| 413 | Upload over `--max-upload-mb` |
| 415 / 400 | Not JSON / malformed JSON |
| 422 | Invalid request: unknown field, bad value, missing media file, unknown device, no such announced stream |

## Routes

| Method | Path | |
|---|---|---|
| GET | `/health` | `{"ok": true, "version": "..."}` |
| GET | `/devices` | `{"host": "ALSA", "devices": [{"name", "kind": "input"\|"output", "default"}]}` |
| GET | `/streams` | Streams announced via SAP (below) |
| GET | `/monitor` | Latest monitor snapshot (below); 404 with `--no-monitor` |
| GET | `/jobs` | All jobs: running ones and the last 50 that ended |
| GET | `/jobs/{id}` | One job |
| DELETE | `/jobs/{id}` | Stop a job; waits up to 3 s and returns it (`stopping` if still winding down) |
| POST | `/senders` | Start sending (body: send request). `201` with the job |
| POST | `/receivers` | Start receiving (body: receive request). `201` with the job |
| POST | `/texts` | Join a text channel (body: text request). `201` with the job |
| POST | `/jobs/{id}/text` | `{"text": "..."}`: send on a text channel, a line per line. Returns the lines |
| GET | `/jobs/{id}/text` | Recent lines on a text channel (up to 200), oldest first |
| POST | `/file-senders` | Send media files (body: file send request). `201` with the job |
| POST | `/file-receivers` | Receive files into the media directory (body: file receive request). `201` with the job |
| GET | `/media` | `[{"name", "bytes", "modified_ms"}]` |
| GET | `/media/{name}` | Download a file. Pictures and audio come with their type; anything else downloads as `application/octet-stream`, so nothing received can run in a browser |
| PUT | `/media/{name}` | Upload a file (raw body). Replaces any file of that name |
| DELETE | `/media/{name}` | Delete a file |
| GET | `/phonebook` | Saved groups, by name: `[{"name", "group", "description"}]` |
| PUT | `/phonebook/{name}` | `{"group": "ADDR:PORT", "description": "...", "name": "new name"}`: add or replace an entry (`name` renames it; `description` and `name` are optional). `201` when new, else `200`, with the entry |
| DELETE | `/phonebook/{name}` | Remove an entry; returns it |
| GET | `/events` | Server-sent events (below) |
| GET | `/probes` | Probes (below) |
| GET | `/probes/{id}` | One probe |
| DELETE | `/probes/{id}` | Forget a probe |
| PUT | `/probes/{id}/config` | Give a probe its own configuration (body: probe config); creates it if unknown |
| DELETE | `/probes/{id}/config` | Back to the defaults |
| GET | `/probes/{id}/reports` | Its last 360 reports, oldest first; `?after_ms=T` for those ending after `T` |
| GET | `/probe-hello?nonce=HEX` | No token needed: `{"name", "proofs": [...]}`, HMAC-SHA256 of `mcast-probe-hello:NONCE` under each token the server accepts, as hex, so a probe that found the server by beacon can check it knows its token. Nonces are 16-128 hex digits |
| POST | `/probes/{id}/checkin` | A probe's heartbeat and reports (below); the only route `--probe-token` opens |
| GET, PUT | `/probe-defaults` | The configuration of probes without their own |
| GET | `/alerts` | `{"active": [ALERT...], "recent": [ALERT...]}`: alerts about probes, active ones oldest first, then the last 200 resolved, newest first (below) |
| GET, PUT | `/alert-rules` | When alerts are raised (below). `PUT` returns the rules saved; turning a kind off resolves its active alerts |

In every request below, `group` can be a phonebook name in place of an
address (`"group": "zone-1"`); names match without regard to case. The job
shows the address. The phonebook is the same file `mcast book` edits
(`~/.config/mcast/phonebook.conf`, or `$MCAST_PHONEBOOK`), read on each
request, so changes from the command line apply at once. Names are letters,
digits, spaces, `-`, `_` and `.`, up to 64 characters; descriptions are one line.

`POST /senders` returns once the sender is ready: files are decoded and
loudness-measured first, so a bad file fails the request instead of the job.

## Send request

Every field is optional; the defaults match `mcast audio send`.

| Field | Default | |
|---|---|---|
| `group` | `"239.69.0.1:5004"` | Multicast group and port |
| `interface` | server's `--iface` | `"any"`, IPv4 address, or interface index |
| `ttl` | `16` | |
| `loopback` | `true` | Deliver to receivers on the same host |
| `tos` | | IP TOS byte, e.g. `184` (DSCP EF) |
| `encoding` | `"l16"` | `l16`, `l24`, `pcmu`/`g711u`, `pcma`/`g711a`, `g722`, `opus` |
| `rate`, `channels` | codec's | |
| `ptime_ms`, `payload_type` | codec's | |
| `opus_bitrate_kbps` | `64` | |
| `opus_mode` | follows `mode` | `voice` or `music` |
| `opus_bandwidth` | | `nb`, `wb`, `swb`, `fb` |
| `opus_fec` | `true` | |
| `source` | 440 Hz tone | `{"type": "tone", "frequency": 440, "level_dbfs": -12}`, `{"type": "pad", "level_dbfs": -12}` (a soft chord progression), `{"type": "file", "path": "NAME", "loop": false}`, `{"type": "capture", "device": "USB"}`, or `{"type": "chime"}` (no audio, just the chime(s); the default chime if neither `chime` nor `end_chime` is set) |
| `mode` | `"none"` | `voice`, `music` or `none` |
| `volume_db` | | |
| `chime` | | Chime before the audio: `"default"` (`rising`, or `$MCAST_CHIME`), a built-in (`"rising"`, `"falling"`, `"ding-dong"`, `"arpeggio"`, `"bell"`, `"alert"`), or `{"file": "NAME"}` |
| `chime_gap_ms` | `400` | |
| `end_chime` | | Chime after the audio, before the tail: as `chime`, but `"default"` is `falling` (or `$MCAST_END_CHIME`) |
| `end_chime_gap_ms` | `400` | |
| `lead_in_ms` | `0` | |
| `tail_ms` | `300` | |
| `start_s`, `duration_s` | | |
| `name` | `"<hostname> audio"` | SAP session name |
| `sap` | `true` | Announce with SAP |
| `sap_group` | from scope | |
| `sap_interval_s` | `5` | |
| `rtcp` | `true` | Send RTCP sender reports (and BYE) on the RTP port + 1 |
| `rtcp_interval_s` | `5` | Average time between reports |

File and chime paths are names of uploaded media files. `render` exists in
the request type but is only available from the CLI.

A page from an uploaded file, with a chime and voice processing for phones:

```bash
curl -H "Authorization: Bearer $MCAST_TOKEN" -H "Content-Type: application/json" \
  -d '{"group": "239.192.2.1:10000", "encoding": "g711u", "mode": "voice", "chime": "default",
       "lead_in_ms": 800, "tos": 184, "source": {"type": "file", "path": "page.mp3"}}' \
  http://127.0.0.1:8650/api/v1/senders
```

Just a chime, e.g. from a button or a schedule:

```bash
curl -H "Authorization: Bearer $MCAST_TOKEN" -H "Content-Type: application/json" \
  -d '{"group": "239.192.2.1:10000", "encoding": "g711u", "tos": 184,
       "source": {"type": "chime"}, "chime": "ding-dong"}' \
  http://127.0.0.1:8650/api/v1/senders
```

The chord pad for 30 seconds, to check speakers and levels on another zone:

```bash
curl -H "Authorization: Bearer $MCAST_TOKEN" -H "Content-Type: application/json" \
  -d '{"group": "239.192.2.2:10000", "encoding": "g722", "mode": "music",
       "source": {"type": "pad", "level_dbfs": -18}, "duration_s": 30}' \
  http://127.0.0.1:8650/api/v1/senders
```

**Overlapping senders:** a request that would send to a group (address
*and* port) another job of this server is sending to, whether a page, a
text channel or a file sender, gets `409`. Stop that job first. Senders
elsewhere on the network aren't checked.

## Receive request

| Field | Default | |
|---|---|---|
| `group` | | Join this group; otherwise pick an announced stream |
| `stream` | | Pick the announced stream whose name contains this |
| `wait_s` | `15` | How long to wait for an announcement the server hasn't seen yet |
| `sap_groups` | defaults | SAP groups to listen on while waiting |
| `interface`, `ttl`, `loopback` | | As above |
| `encoding`, `rate`, `channels` | | With `group`: how to decode (G.711/G.722 are detected) |
| `latency_ms` | `40` | Playout delay |
| `volume_db` | | |
| `sink` | default device | `{"type": "play", "device": "Speakers"}` |
| `duration_s` | | |
| `stats_interval_s` | `2` | |
| `rtcp` | `true` | Hear the sender's RTCP and send receiver reports |
| `rtcp_interval_s` | `5` | |
| `name` | hostname | Our name in receiver reports, e.g. the room |

Recording to WAV is CLI-only.

## Text request

A text channel sends what clients post to `/jobs/{id}/text` and reports
everyone else's real-time text as lines (`text_line` events, and
`GET /jobs/{id}/text`). It returns once it's listening. Every field is optional.

| Field | Default | |
|---|---|---|
| `group` | | Channel group; otherwise `stream`, else `"239.69.1.1:5004"` |
| `stream` | | Join the announced text stream whose name contains this |
| `name` | host name | Our name, shown to the others over RTCP |
| `send` | `true` | `false` only listens (and doesn't reserve the group) |
| `sap` | `false` | Announce our text with SAP under `name` |
| `sap_group`, `sap_interval_s` | from scope, `5` | |
| `redundancy` | `true` | Repeat each packet's text in the next two (RFC 2198) |
| `t140_pt`, `red_pt` | `98`, `100` | With `group` |
| `interface`, `ttl`, `loopback`, `tos` | | As for senders |
| `rtcp`, `rtcp_interval_s` | `true`, `5` | |

```bash
curl -H "Authorization: Bearer $MCAST_TOKEN" -H "Content-Type: application/json" \
  -d '{"name": "Front desk"}' http://127.0.0.1:8650/api/v1/texts
curl -H "Authorization: Bearer $MCAST_TOKEN" -H "Content-Type: application/json" \
  -d '{"text": "Fire drill at 3pm"}' http://127.0.0.1:8650/api/v1/jobs/1/text
```

A line is `{"job", "from", "ssrc", "text", "own", "at_ms"}`: `from` is the
sender's RTCP name (or address until it's known), and `own` marks lines
typed here.

## File send request

Sends media files one after another; the last can loop so late joiners get
it too. Files are read before anything is sent, so a missing one fails the
request.

| Field | Default | |
|---|---|---|
| `files` | (required) | Media file names |
| `group` | `"239.69.2.1:5004"` | |
| `rate_kbps` | `2000` | Keep it modest on networks that also carry voice |
| `redundancy` | `0.5` | Extra repair data per file (0.5 sends 1.5x the file) |
| `loop` | `false` | Keep sending the last file until stopped |
| `name` | `"<hostname> files"` | SAP session name |
| `sap`, `sap_group`, `sap_interval_s` | `true`, from scope, `5` | |
| `payload_type` | `110` | |
| `interface`, `ttl`, `loopback`, `tos`, `rtcp`, `rtcp_interval_s` | | As for senders |

## File receive request

Saves each file that arrives (and passes its checksum) into the media
directory under a safe name: no directories or hidden names, and `-2`,
`-3`... instead of replacing a file already there. Received files can be
downloaded, paged, or sent on. Each one is reported as a `file_received`
event.

| Field | Default | |
|---|---|---|
| `group` | | Otherwise `stream`, else `"239.69.2.1:5004"` |
| `stream` | | The announced file stream whose name contains this |
| `max_size_mb` | 100, or `--max-upload-mb` if smaller | Refuse larger files; can't exceed `--max-upload-mb` |
| `payload_type` | `110` | With `group` |
| `name` | host name | Our name in RTCP receiver reports |
| `interface`, `rtcp`, `rtcp_interval_s` | | |

## Jobs

```json
{
  "id": 1, "kind": "send", "state": "running",
  "group": "239.192.2.1:10000", "name": "Lobby page",
  "summary": "PCMU/8000/1 sending to 239.192.2.1:10000 (pt 0, 20 ms packets, voice processing, rising chime)",
  "notes": ["loudness: measured -11.0 LUFS after processing; -5.0 dB to reach -16 LUFS"],
  "created_ms": 1794022911066, "ended_ms": null, "error": null,
  "stats": {"packets": 204, "send_errors": 0, "seconds": 4.08, "capture_dropped": 0},
  "request": { "...": "the request as sent" }
}
```

`kind` is `send` (audio), `recv` (audio), `text`, `file_send` or
`file_recv`. `state` is `starting`, `running`, `stopping`, `finished` (source ended),
`stopped` (on request) or `failed` (see `error`). Receive jobs' `stats` are
`packets`, `lost`, `concealed`, `late`, `invalid`, `underruns`, `overflows`,
`buffered_ms`, and `transit_ms` (from the sender's RTCP: time from the
sender's clock to arrival, `null` until a report arrives; only a true delay
when both clocks are synced). The sender's RTCP name and BYE appear in
`notes`.

Text jobs' `stats`: `lines_sent`, `lines_received`, `chars_sent`,
`packets_sent`, `senders`. File send: `file`, `index` and `count` (which
file of how many), `sent`, `needed` (packets that hold the file) and
`total` (`null` while looping), `looping`. File receive: `receiving` and
`percent` (the file arriving now), `received`, `failed`.

## Streams

```json
{
  "name": "Lobby page", "origin": "192.0.2.2", "from": "192.0.2.2:41000",
  "audio": {"group": "239.192.2.1:10000", "payload_type": 0, "encoding": "PCMU",
            "sample_rate": 8000, "channels": 1, "ptime_ms": 20.0, "playable": true},
  "text": null, "file": null,
  "sdp": "v=0\r\n..."
}
```

A text stream has `"text": {"group", "t140_pt", "red_pt"}` and a file
stream `"file": {"group", "payload_type"}`; join or receive them by `stream`
name.

## Monitor

`{"groups": [...], "streams": [...]}`. Each stream row: `group`, `name`,
`source`, `ssrc`, `payload_type`, `codec`, `packets`, `bytes`,
`packets_per_sec`, `kbit_per_sec`, `lost`, `late`, `loss_percent`,
`jitter_ms`, `max_gap_ms`, `ptime_ms`, `ttl`, `tos`, `peak_dbfs`, `rms_dbfs`
(`null` when silent), `idle_ms`. Rates and levels cover the last second.

## Probes

See [probes.md](probes.md) for what probes are and how to use them.

A probe config (`PUT /probes/{id}/config`, `/probe-defaults`). A `PUT`
replaces the whole config. A missing field takes the default in this table,
not its current value (or the probe defaults), so send every field you
want to keep. Unknown fields are refused:

| Field | Default | |
|---|---|---|
| `label` | `null` | Shown in listings and alerts (at most 100 characters) |
| `sap` | `true` | Watch every SAP-announced stream |
| `groups` | `[]` | Also watch these multicast groups (`"ADDR:PORT"`) |
| `report_interval_s` | `10` | Seconds per report and check-in (1 to 3600) |
| `unicast` | `false` | Send this probe a unicast copy of every page this server sends |

A probe (`GET /probes/{id}`): `id`, `online` (it checked in within three
report intervals plus 5 s), `config` (the one it should run), `custom_config` (its own rather than the defaults), `config_applied`
(its last check-in said it runs `config`), `hostname`, `version`,
`unicast_addr`, `first_seen_ms`, `last_seen_ms`, `uptime_s`, `queued`
(reports still waiting on the probe), `dropped` (reports it discarded while
the server was unreachable), `latest` (its last report), `comparisons` and
`totals`.

A report: `seq`, `start_ms`, `end_ms`, `streams` (monitor rows, as in
[Monitor](#monitor), but covering the report interval; the unicast port
shows as a group like `0.0.0.0:5104`) and `groups` (the groups watched, with
any `error` joining one).

A comparison, from the latest report, for each unicast copy: `ssrc`, `name`,
`group` (the multicast group, if seen), `multicast` and `unicast`
(`{"packets", "lost", "loss_percent", "jitter_ms", "max_gap_ms"}`;
`multicast` is `null` if it wasn't seen) and `verdict`: `ok`,
`multicast_loss`, `unicast_loss`, `both_loss`, `no_multicast` or `idle`.

A total, per stream since the server first saw the probe (since it started,
or since the probe was forgotten): `group`, `unicast`,
`name`, `ssrc`, `packets`, `lost`, `late`, `loss_percent`, `intervals`,
`lossy_intervals`, `max_gap_ms`, `worst_jitter_ms`, `last_seen_ms`.

### Check-in

`POST /probes/{id}/checkin`, from `mcast probe run`. IDs are 1-64 letters,
digits, `.`, `_` or `-`; a check-in from an unknown ID adds the probe.

```json
{"version": "0.2.0", "hostname": "pi-idf3", "boot": 3735928559, "uptime_s": 120.5,
 "config": {"label": null, "sap": true, "groups": [], "report_interval_s": 10, "unicast": true},
 "unicast_addr": "10.0.3.17:5104",
 "reports": [{"seq": 12, "start_ms": 1794026259854, "end_ms": 1794026269854, "streams": [], "groups": []}],
 "queued": 0, "dropped": 0}
```

Only `version` and `boot` are required. `boot` is random for each run of the
probe and `seq` counts its reports from 1, so reports the server already has
are ignored and retries are safe. `config` is what the probe runs (`null`
before its first reply, or while it can't start monitoring), `unicast_addr` where it wants unicast copies
(`null`: none), `queued` how many more reports it still holds, and `dropped`
how many it discarded because its queue was full. At most 360 reports per
check-in (`mcast probe run` sends 30). The reply:

```json
{"config": {...}, "acked_seq": 12, "server_time_ms": 1794026269854}
```

### Alerts

See [probes.md](probes.md#alerts) for when alerts are raised and resolved.
An alert:

| Field | |
|---|---|
| `id` | Counts up from 1 for each run of the server |
| `kind` | `probe_offline`, `multicast_loss` or `no_multicast` |
| `probe` | The probe's ID |
| `label` | The probe's label when the alert was raised, or `null` |
| `stream` | The stream's name, or its group if it has none; `null` for `probe_offline` |
| `group` | The multicast group, or `null` |
| `message` | One line for people, e.g. "idf-3 (3rd floor IDF): 2.4% multicast loss on Lobby; its unicast copy is clean, so look at the multicast path (...)" |
| `raised_ms` | When it started: the report that crossed the threshold, or the probe's last check-in |
| `resolved_ms` | When it ended, or `null` while active |
| `worst_loss_percent` | `multicast_loss`: the worst interval's loss while active |
| `unicast_clean` | `multicast_loss`, in the interval that raised it: `true` if the stream's unicast copy arrived clean, `false` if it lost packets too, `null` if no copy of this stream arrived (the probe doesn't get copies, or this server doesn't send the stream) |

Alert rules (`GET`/`PUT /alert-rules`). Every field is optional in a `PUT`
(missing ones take the defaults) and unknown fields are refused:

| Field | Default | |
|---|---|---|
| `offline` | `true` | Alert when a probe stops checking in |
| `multicast_loss` | `true` | Alert on multicast loss |
| `loss_percent` | `1` | Loss in one report interval that counts as bad (more than 0, at most 100) |
| `no_multicast` | `true` | Alert when a stream's unicast copy arrives but the stream doesn't |
| `raise_after` | `2` | Bad intervals in a row before an alert is raised (1 to 100) |
| `clear_after` | `3` | Clean intervals in a row before it's resolved (1 to 100) |

With `--alert-webhook URL`, each raised or resolved alert is also POSTed to
the URL as `{"text": "ALERT: MESSAGE" or "RESOLVED: MESSAGE", "event":
"alert_raised" or "alert_resolved", "alert": ALERT}`; see
[probes.md](probes.md#alerts).

## Events

`GET /events` is a `text/event-stream`. Each event's name is its `type`, and
its data is JSON with that `type` too:

| Type | Data |
|---|---|
| `job_started` | `{"job": JOB}` |
| `job_updated` | `{"job": JOB}`, about once a second while running, and on stop requests |
| `job_ended` | `{"job": JOB}` |
| `stream_added`, `stream_changed` | `{"stream": STREAM}` |
| `stream_removed` | `{"stream": STREAM, "reason": "deleted"\|"expired"}` |
| `monitor` | `{"snapshot": SNAPSHOT}`, every second |
| `probe_updated` | `{"probe": PROBE}`: it checked in, went offline, or was reconfigured |
| `probe_removed` | `{"id": "..."}` |
| `alert_raised`, `alert_resolved` | `{"alert": ALERT}` |
| `text_line` | `{"line": LINE}`: a line received on a text channel, or typed into one |
| `file_received` | `{"job": ID, "file": {"name", "sent_as", "bytes", "content_type", "verified", "from", "at_ms"}}` |
| `phonebook_changed` | `{}`: the phonebook was changed through the API; fetch it again |
| `lagged` | `{"missed": N}`: the client fell behind; refetch the lists |

Fetch the lists first, then apply events. For example:

```js
const es = new EventSource("/api/v1/events?access_token=" + token);
es.addEventListener("job_ended", e => console.log(JSON.parse(e.data).job));
```
