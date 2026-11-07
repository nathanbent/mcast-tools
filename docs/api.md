# mcast API

`mcast serve` exposes sending, receiving, discovery and monitoring over
HTTP/JSON. Every route is under `/api/v1`.

```bash
mcast serve [--listen 127.0.0.1:8650] [--token TOKEN] [--media-dir DIR]
            [--max-upload-mb 200] [--iface IFACE] [--sap-group ADDR:PORT]...
            [--watch ADDR:PORT]... [--no-monitor]
            [--probe-token TOKEN] [--probe-state FILE]
            [--no-beacon] [--beacon-group ADDR:PORT] [--beacon-url URL] [--beacon-ttl 16]
```

Ctrl-C or SIGTERM stops every job (withdrawing their SAP announcements) before exiting.

## Authentication

With `--token` (or `MCAST_TOKEN`), every request needs
`Authorization: Bearer TOKEN`. Browsers' `EventSource` can't set headers, so
`?access_token=TOKEN` is accepted too. The server won't start on a
non-loopback address without a token. `--probe-token` (or
`MCAST_PROBE_TOKEN`) is a second token that only opens
`POST /probes/{id}/checkin`, for the probes. It speaks plain HTTP: beyond a trusted
LAN, put it behind a TLS reverse proxy.

## Errors

Errors are JSON: `{"error": "message"}`.

| Status | When |
|---|---|
| 401 | Missing or wrong token |
| 404 | No such job, media file or route |
| 409 | A page to a group already being sent to (adds `"job": ID`, the job sending there) |
| 413 | Upload over `--max-upload-mb` |
| 415 / 400 | Not JSON / malformed JSON |
| 422 | Invalid request: unknown field, bad value, missing media file, unknown device |

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
| GET | `/media` | `[{"name", "bytes", "modified_ms"}]` |
| PUT | `/media/{name}` | Upload a file (raw body). Replaces any file of that name |
| DELETE | `/media/{name}` | Delete a file |
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
| `source` | 440 Hz tone | `{"type": "tone", "frequency": 440, "level_dbfs": -12}`, `{"type": "pad", "level_dbfs": -12}` (a soft chord progression), `{"type": "file", "path": "NAME", "loop": false}`, or `{"type": "capture", "device": "USB"}` |
| `mode` | `"none"` | `voice`, `music` or `none` |
| `volume_db` | | |
| `chime` | | `"default"` (built-in, or `$MCAST_CHIME`) or `{"file": "NAME"}` |
| `chime_gap_ms` | `400` | |
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

**Overlapping pages:** a send request to a group (address *and* port) that
another job of this server is sending to gets `409`. Stop that job first.
Senders elsewhere on the network aren't checked.

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

## Jobs

```json
{
  "id": 1, "kind": "send", "state": "running",
  "group": "239.192.2.1:10000", "name": "Lobby page",
  "summary": "PCMU/8000/1 sending to 239.192.2.1:10000 (pt 0, 20 ms packets, voice processing, built-in chime)",
  "notes": ["loudness: measured -11.0 LUFS after processing; -5.0 dB to reach -16 LUFS"],
  "created_ms": 1794022911066, "ended_ms": null, "error": null,
  "stats": {"packets": 204, "send_errors": 0, "seconds": 4.08, "capture_dropped": 0},
  "request": { "...": "the request as sent" }
}
```

`state` is `starting`, `running`, `stopping`, `finished` (source ended),
`stopped` (on request) or `failed` (see `error`). Receive jobs' `stats` are
`packets`, `lost`, `concealed`, `late`, `invalid`, `underruns`, `overflows`,
`buffered_ms`, and `transit_ms` (from the sender's RTCP: time from the
sender's clock to arrival, `null` until a report arrives; only a true delay
when both clocks are synced). The sender's RTCP name and BYE appear in
`notes`.

## Streams

```json
{
  "name": "Lobby page", "origin": "192.0.2.2", "from": "192.0.2.2:41000",
  "audio": {"group": "239.192.2.1:10000", "payload_type": 0, "encoding": "PCMU",
            "sample_rate": 8000, "channels": 1, "ptime_ms": 20.0, "playable": true},
  "sdp": "v=0\r\n..."
}
```

## Monitor

`{"groups": [...], "streams": [...]}`. Each stream row: `group`, `name`,
`source`, `ssrc`, `payload_type`, `codec`, `packets`, `bytes`,
`packets_per_sec`, `kbit_per_sec`, `lost`, `late`, `loss_percent`,
`jitter_ms`, `max_gap_ms`, `ptime_ms`, `ttl`, `tos`, `peak_dbfs`, `rms_dbfs`
(`null` when silent), `idle_ms`. Rates and levels cover the last second.

## Probes

See [probes.md](probes.md) for what probes are for. A probe config:

| Field | Default | |
|---|---|---|
| `label` | | Shown in listings |
| `sap` | `true` | Watch every SAP-announced stream |
| `groups` | `[]` | Also watch these multicast groups |
| `report_interval_s` | `10` | Seconds per report and check-in (1 to 3600) |
| `unicast` | `false` | Send this probe a unicast copy of every sender job's packets |

A probe (`GET /probes/{id}`): `id`, `online`, `config` (the one it should
run), `custom_config` (its own rather than the defaults), `config_applied`
(its last check-in said it runs `config`), `hostname`, `version`,
`unicast_addr`, `first_seen_ms`, `last_seen_ms`, `uptime_s`, `queued`
(reports still waiting on the probe), `dropped` (reports it discarded while
the server was unreachable), `latest` (its last report), `comparisons` and
`totals`.

A report: `seq`, `start_ms`, `end_ms`, `streams` (monitor rows, as below,
covering the interval; the unicast port shows as a group like
`0.0.0.0:5104`) and `groups`.

A comparison, from the latest report, for each unicast copy: `ssrc`, `name`,
`group` (the multicast group, if seen), `multicast` and `unicast`
(`{"packets", "lost", "loss_percent", "jitter_ms", "max_gap_ms"}`;
`multicast` is `null` if it wasn't seen) and `verdict`: `ok`,
`multicast_loss`, `unicast_loss`, `both_loss`, `no_multicast` or `idle`.

A total, per stream since the probe was first seen: `group`, `unicast`,
`name`, `ssrc`, `packets`, `lost`, `late`, `loss_percent`, `intervals`,
`lossy_intervals`, `max_gap_ms`, `worst_jitter_ms`, `last_seen_ms`.

### Check-in

`POST /probes/{id}/checkin`, from `mcast probe run`. IDs are 1-64 letters,
digits, `.`, `_` or `-`.

```json
{"version": "0.1.1", "hostname": "pi-idf3", "boot": 3735928559, "uptime_s": 120.5,
 "config": {...}, "unicast_addr": "10.0.3.17:5104",
 "reports": [{"seq": 12, "start_ms": ..., "end_ms": ..., "streams": [...]}],
 "queued": 0, "dropped": 0}
```

`boot` is random for each run of the probe and `seq` counts its reports from
1; reports the server already has are ignored, so retries are safe.
`config` is what the probe runs (`null` before its first reply). The reply:

```json
{"config": {...}, "acked_seq": 12, "server_time_ms": 1794026269854}
```

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
| `lagged` | `{"missed": N}`: the client fell behind; refetch the lists |

Fetch the lists first, then apply events. For example:

```js
const es = new EventSource("/api/v1/events?access_token=" + token);
es.addEventListener("job_ended", e => console.log(JSON.parse(e.data).job));
```
