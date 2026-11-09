# mcast API

`mcast serve` exposes sending, receiving, discovery and monitoring over
HTTP/JSON. Every route is under `/api/v1`.

```bash
mcast serve [--listen 127.0.0.1:8650] [--token TOKEN] [--media-dir DIR]
            [--max-upload-mb 200] [--iface IFACE] [--sap-group ADDR:PORT]...
            [--watch ADDR:PORT]... [--no-monitor]
```

`--listen` can also be set with `MCAST_LISTEN`, and `--token` with
`MCAST_TOKEN`; a flag given on the command line wins.

Ctrl-C or SIGTERM stops every job (withdrawing their SAP announcements) before exiting.

## Authentication

With `--token` (or `MCAST_TOKEN`), every request needs
`Authorization: Bearer TOKEN`. Browsers' `EventSource` can't set headers, so
`?access_token=TOKEN` is accepted too. The server won't start on a
non-loopback address without a token. It speaks plain HTTP: beyond a trusted
LAN, put it behind a TLS reverse proxy.

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
| POST | `/jobs/{id}/live` | For a sender with a `live` source: the request body, as it arrives, is the page's audio, 16-bit little-endian PCM at the source's rate and channels (send it chunked). When the body ends, the page plays what's left, then its end chime and tail, and finishes. Returns `{"bytes"}` then, or `409` if the job was stopped first |
| POST | `/jobs/{id}/text` | `{"text": "..."}`: send on a text channel, a line per line. Returns the lines |
| GET | `/jobs/{id}/text` | Recent lines on a text channel (up to 200), oldest first |
| POST | `/file-senders` | Send media files (body: file send request). `201` with the job |
| POST | `/file-receivers` | Receive files into the media directory (body: file receive request). `201` with the job |
| GET | `/media` | Every file, in every folder: `[{"name", "bytes", "modified_ms"}]`, `name` being its path (`bells/lunch.wav`). With `?audio=1`, each also has `audio` (`{"codec", "rate", "channels", "duration_s"}`) if it's audio mcast can play, else `audio_error` saying why not. Files are probed once and remembered until they change |
| GET | `/media-info/{name}` | One file, as in `/media?audio=1` |
| GET | `/media/{name}` | Download a file. Pictures and audio come with their type; anything else downloads as `application/octet-stream`, so nothing received can run in a browser |
| PUT | `/media/{name}` | Upload a file (raw body), making any folders in its path. Replaces any file of that name. With `?audio=1`, it must be audio mcast can play: if not, `422` and any old file stays. Returns `{"name", "bytes"}`, and `audio` when checked |
| DELETE | `/media/{name}` | Delete a file |
| GET | `/media-folders` | Every folder, empty ones included: `["bells", "bells/old"]` |
| POST | `/media-folders` | `{"name": "bells/old"}`: make a folder (and those above it) |
| DELETE | `/media-folders/{name}` | Delete a folder; `409` unless it's empty |
| POST | `/media-move` | `{"from", "to"}`: rename or move a file, or a folder with everything in it. Makes the folders above `to`; `409` if `to` exists. Returns `{"from", "to", "folder"}` |
| GET | `/phonebook` | Saved groups, by name: `[{"name", "group", "description"}]` |
| PUT | `/phonebook/{name}` | `{"group": "ADDR:PORT", "description": "...", "name": "new name"}`: add or replace an entry (`name` renames it; `description` and `name` are optional). `201` when new, else `200`, with the entry |
| DELETE | `/phonebook/{name}` | Remove an entry; returns it |
| GET | `/chimes` | The built-in chimes, grouped by kind: `[{"name", "category", "description", "hidden"}]`; `category` is `chime`, `bell`, `tone` or `alert` |
| PUT | `/chimes` | `{"hidden": ["klaxon", ...]}`: hide exactly these from lists (and show the rest); returns the list |
| PUT | `/chimes/{name}` | `{"hidden": true}` or `false`: hide or show one; returns it |
| GET | `/chimes/{name}/preview` | The chime as a WAV file (24 kHz mono), to listen to before choosing it |
| GET | `/voices` | The text-to-speech engine and its voices (see Text to speech) |
| GET | `/events` | Server-sent events (below) |

In every request below, `group` can be a phonebook name in place of an
address (`"group": "zone-1"`); names match without regard to case. The job
shows the address. The phonebook is the same file `mcast book` edits
(`~/.config/mcast/phonebook.conf`, or `$MCAST_PHONEBOOK`), read on each
request, so changes from the command line apply at once. Names are letters,
digits, spaces, `-`, `_` and `.`, up to 64 characters; descriptions are one line.

Hiding a chime only takes it out of the lists clients offer: a request that
names a hidden chime still plays it. Which are hidden is kept in
`chimes.json` beside the phonebook (or `$MCAST_CHIMES`), shared with
`mcast audio chimes hide` / `show`.

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
| `source` | 440 Hz tone | `{"type": "tone", "frequency": 440, "level_dbfs": -12}`, `{"type": "pad", "level_dbfs": -12}` (a soft chord progression), `{"type": "file", "path": "NAME", "loop": false}`, `{"type": "files", "paths": ["NAME", ...], "gap_ms": 0}` (up to 100 files one after another as one stream, at the first one's rate and channels, with `gap_ms` of silence between them; loudness is measured over them all; an item can be text to speak instead, `{"say": "TEXT", "voice": "NAME"}`: see Text to speech; or a pause, `{"silence_ms": 5000}`, up to 60000, which is an item of its own in `stats.item`), `{"type": "capture", "device": "USB"}`, `{"type": "chime"}` (no audio, just the chime(s); the default chime if neither `chime` nor `end_chime` is set), or `{"type": "live", "rate": 48000, "channels": 1}` (audio streamed in with `POST /jobs/{id}/live`; the defaults are shown) |
| `mode` | `"none"` | `voice`, `music` or `none` |
| `volume_db` | | Volume change after processing, with a limiter; chimes too |
| `input_gain_db` | | Gain on the source before processing (not the chimes), -40 to 40, e.g. `12` for a quiet microphone; a boost is limited |
| `normalize` | `false` | Normalise loudness (to -16 LUFS) even with mode `none`; `voice` and `music` always do. Files are measured first; live and capture input uses an AGC that settles within about a second |
| `chime` | | Chime before the audio: `"default"` (`rising`, or `$MCAST_CHIME`), a built-in (any name `GET /chimes` lists, such as `"ding-dong"`, `"westminster"`, `"church-bell"`, `"beep"` or `"siren-wail"`), or `{"file": "NAME"}` |
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

File and chime paths are names of uploaded media files, with their folders (`bells/lunch.wav`). `render` exists in
the request type but is only available from the CLI.

### Text to speech

A `files` source's items can be text: `{"say": "The library closes in ten
minutes"}`, with an optional `voice`. The server speaks each into a WAV
before the sender starts (so `POST /senders` takes longer the first time),
and keeps it in a cache, so the same words in the same voice are spoken
once. Up to 2000 characters per item; the job's `request` shows the text.

The engine is found when the server starts:

1. `--tts-command CMD` (`MCAST_TTS_COMMAND`): run with `sh -c`, the text on
   stdin, a WAV on stdout, the voice asked for in `$MCAST_TTS_VOICE`.
   A command can't be asked which voices it has, so list them with
   `--tts-voice-list a,b,c` (`MCAST_TTS_VOICE_LIST`) for `GET /voices` to
   offer. Without `--tts-voice`, the command picks the voice when a request
   names none.
2. Piper, if `piper` is on the `PATH` and `--tts-voices DIR` (default
   `/usr/local/share/mcast/voices`) holds at least one voice (`NAME.onnx`
   with its `NAME.onnx.json`). `voice` names one; the default is
   `--tts-voice`, else the first by name.
3. eSpeak NG (`espeak-ng`), whose `voice` is a language such as `en-us`.

Without one, a request with text to speak fails with `400`, as does text
the engine can't speak. `GET /voices` says which engine there is and its
voices: `{"engine": "piper"|"espeak-ng"|"command"|null, "voices": [...],
"default_voice"}`.

Each item must be spoken within `--tts-timeout` seconds (`MCAST_TTS_TIMEOUT`,
60 by default, up to 3600), whatever the engine; a slower one fails the
request. `POST /senders` answers only once every item is spoken, so a client
should wait at least that long per text item (the intercom panel waits up to
30 minutes).

A live page: start a sender with a `live` source, then stream audio into
it. Audio that arrives during the opening chime is kept and played after it.
Gaps are filled with silence; if nothing arrives for 20 s the page ends.

```bash
curl -s -H "Authorization: Bearer $MCAST_TOKEN" http://127.0.0.1:8650/api/v1/senders \
  -d '{"group": "239.192.2.1:10000", "encoding": "g711u", "mode": "voice", "chime": "default",
       "end_chime": "default", "source": {"type": "live", "rate": 16000}}'
arecord -q -f S16_LE -r 16000 -c 1 | curl -s -T - -X POST -H "Authorization: Bearer $MCAST_TOKEN" \
  http://127.0.0.1:8650/api/v1/jobs/ID/live
```

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
  "group": "239.192.2.1:10000", "name": "Lobby page", "ssrc": 2864434397,
  "summary": "PCMU/8000/1 sending to 239.192.2.1:10000 (pt 0, 20 ms packets, voice processing, rising chime)",
  "notes": ["loudness: measured -11.0 LUFS after processing; -5.0 dB to reach -16 LUFS"],
  "created_ms": 1794022911066, "ended_ms": null, "error": null,
  "stats": {"packets": 204, "send_errors": 0, "seconds": 4.08, "capture_dropped": 0},
  "request": { "...": "the request as sent" }
}
```

`kind` is `send` (audio), `recv` (audio), `text`, `file_send` or
`file_recv`. `state` is `starting`, `running`, `stopping`, `finished` (source ended),
`stopped` (on request) or `failed` (see `error`). `ssrc` is set on audio
send jobs once they start: the RTP SSRC of the stream, which listeners'
reports in the monitor name as `about`. It's `null` on other jobs. A
sender with a `files` source has `item` in its `stats` while it plays: the
index of the item playing, from when the first one starts (after the
chime), with a `job_updated` event as each starts. Receive jobs' `stats` are
`packets`, `lost`, `concealed`, `late` (arrived too late to play),
`reordered` (arrived out of order but in time: put back in order and
played), `invalid`, `underruns`, `overflows`, `buffered_ms`, `drift_ppm`
(the sender's clock against the sound card's, positive when the sender is
fast, as measured and corrected for; `null` unless playing), and
`transit_ms` (from the sender's RTCP: time from the sender's clock to
arrival, `null` until a report arrives; only a true delay when both clocks
are synced). The sender's RTCP name and BYE appear in
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

`{"groups": [...], "streams": [...], "listeners": [...]}`. Each stream row:
`group`, `name`, `source`, `ssrc`, `payload_type`, `codec`, `packets`, `bytes`,
`packets_per_sec`, `kbit_per_sec`, `lost`, `late`, `loss_percent`,
`jitter_ms`, `max_gap_ms`, `ptime_ms`, `ttl`, `tos`, `peak_dbfs`, `rms_dbfs`
(`null` when silent), `idle_ms`. Rates and levels cover the last second.

`listeners` are receivers heard sending RTCP receiver reports on a watched
group, such as speakers running `mcast receive --name Lobby`. Each row:
`group`, `ssrc`, `from`, `name` (its `--name`, else its hostname), `cname`,
`about` (the `ssrc` of the stream it's hearing, matching a stream row's, or
`null` when it hears nothing), `last_heard_ms`, `left` (it said BYE),
`loss_percent` (since its previous report), `cumulative_lost`, `jitter_ms`,
and for mcast-tools receivers `transit_ms`, `buffered_ms` and `latency_ms`
(how far behind the sender it plays). A listener is dropped after about 30 s
without a report. Receivers send reports every few seconds (5 s by default)
while they're running, so a listener's `about` shows which page it heard.

## Events

`GET /events` is a `text/event-stream`. Each event's name is its `type`, and
its data is JSON with that `type` too:

| Type | Data |
|---|---|
| `job_started` | `{"job": JOB}` |
| `job_updated` | `{"job": JOB}`, about once a second while running, on stop requests, and as each item of a `files` source starts |
| `job_ended` | `{"job": JOB}` |
| `stream_added`, `stream_changed` | `{"stream": STREAM}` |
| `stream_removed` | `{"stream": STREAM, "reason": "deleted"\|"expired"}` |
| `monitor` | `{"snapshot": SNAPSHOT}`, every second |
| `text_line` | `{"line": LINE}`: a line received on a text channel, or typed into one |
| `text_renamed` | `{"job", "ssrc", "name"}`: a sender's name (from RTCP) arrived after lines from it went out labelled with its address; those lines on channel `job` from `ssrc` (not your own) are now `name`'s. The lines `GET /jobs/{id}/text` returns are relabelled too |
| `file_received` | `{"job": ID, "file": {"name", "sent_as", "bytes", "content_type", "verified", "from", "at_ms"}}` |
| `phonebook_changed` | `{}`: the phonebook was changed through the API; fetch it again |
| `chimes_changed` | `{}`: chimes were hidden or shown through the API; fetch `/chimes` again |
| `media_changed` | `{}`: media files or folders were uploaded, deleted, moved or made through the API; fetch `/media` (and `/media-folders`) again |
| `lagged` | `{"missed": N}`: the client fell behind; refetch the lists |

Fetch the lists first, then apply events. For example:

```js
const es = new EventSource("/api/v1/events?access_token=" + token);
es.addEventListener("job_ended", e => console.log(JSON.parse(e.data).job));
```
