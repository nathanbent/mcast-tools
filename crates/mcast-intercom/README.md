# mcast-intercom

An intercom control panel for `mcast serve`. You set up **zones** (sets of speakers)
and **announcements** (an audio file or a chime, which zones it goes to, how
many times, at what priority), then play them from a button or on a
**schedule**.

It is a client of the mcast HTTP API and nothing else: it doesn't use any
other crate in this workspace, so it can be moved out on its own. See
[`docs/intercom-plan.md`](../../docs/intercom-plan.md) for how it came about.

## Use

`mcast serve` must be running. The intercom talks to it at `--url`
(`$MCAST_URL`, default `http://127.0.0.1:8650`) with `--token` (`$MCAST_TOKEN`).

```bash
mcast intercom list            # zones and announcements
mcast intercom check           # the config, and its media files and phonebook names on the server
mcast intercom upcoming        # what the schedules play next
mcast intercom play lunch      # by id or name; Ctrl-C stops it
```

### The panel

```bash
mcast intercom serve                                          # open http://127.0.0.1:8651/
MCAST_INTERCOM_TOKEN=change-me mcast intercom serve --listen 0.0.0.0:8651
```

A page with a big button for each announcement (Stop and an *All stop* in
the header), what's playing in each group right now and what's next, the
history of recent plays, and editors for schedules, announcements and zones. Media files and phonebook
names come from the mcast server; upload files through `mcast serve`.

Schedules only play while `mcast intercom serve` runs.

The panel is its own server with its own token (`--panel-token` or
`$MCAST_INTERCOM_TOKEN`, required unless it listens on loopback). `--token`
stays the mcast server's. It saves edits to `intercom.json`, reading the file
again first, so changes made by hand while it runs aren't lost. It keeps the
last 50 plays in memory. When it stops, anything playing finishes on the mcast
server, but repeats still to come are dropped.

On a Raspberry Pi, `ENABLE_INTERCOM=yes` in `pi/pi.conf` sets it up as a
service next to `mcast serve` ([pi/README.md](../../pi/README.md#the-intercom-panel)).

`--web-dir crates/mcast-intercom/assets` serves the page from disk, so a
reload picks up edits; `--no-web` serves the API alone.

## Config

`intercom.json` in the mcast config directory (`~/.config/mcast`), or the
file named by `--config` or `$MCAST_INTERCOM`. Edit it in the panel or by hand;
`mcast intercom check` tells you about mistakes.

```json
{
  "zones": [
    {"id": "upstairs", "name": "Upstairs", "targets": [
      {"group": "zone-1", "encoding": "g711u", "tos": 184},
      {"group": "239.192.2.2:10000", "encoding": "l16", "volume_db": -3}
    ]},
    {"id": "downstairs", "name": "Downstairs", "targets": [
      {"group": "zone-2", "encoding": "g722"}
    ]}
  ],
  "announcements": [
    {"id": "lunch", "name": "Lunch bell", "audio": {"media": "lunch.wav"},
     "chime": "default", "mode": "voice", "repeat": 2, "repeat_gap_s": 3,
     "zones": ["upstairs", "downstairs"]},
    {"id": "fire", "name": "Fire drill", "audio": "chime_only", "chime": "alert",
     "repeat": 5, "zones": ["upstairs", "downstairs"], "priority": "emergency"}
  ],
  "schedules": [
    {"id": "lunch", "announcement": "lunch",
     "when": {"weekly": {"days": ["mon", "tue", "wed", "thu", "fri"], "times": ["12:00", "12:45"]}}},
    {"id": "drill", "announcement": "fire", "when": {"once": "2026-12-04T10:30"}, "enabled": false}
  ],
  "time_zone": "Europe/London"
}
```

**Zones.** `id` (letters, digits, `-`, `_`), `name`, and `targets`. Each target is a
multicast `group` (a phonebook name or an address) plus the send options
its speakers need: `encoding`, `volume_db` (added to the announcement's),
`tos`, `ttl`, `interface`. Leave an option out to use the server's default.

**Announcements.** `id`, `name` and `zones` (zone ids), then:

| Field | Default | |
|---|---|---|
| `audio` | | `{"media": "NAME"}`, a file uploaded to the server, or `"chime_only"` |
| `chime`, `end_chime` | none | As in the API: `"default"`, a built-in (`"ding-dong"`, `"bell"`, `"alert"`, ...), or `{"file": "NAME"}`. With `"chime_only"` and neither set, the default chime |
| `mode` | | `voice`, `music` or `none` |
| `volume_db`, `lead_in_ms` | | |
| `repeat` | `1` | Times to play it (up to 20); each repeat has its chimes |
| `repeat_gap_s` | `2` | Seconds between repeats |
| `priority` | `normal` | What happens to a group something else is already sending to (below) |

**Schedules.** `id`, `announcement` (its id), `enabled` (default `true`),
an optional `name`, and `when`: `{"weekly": {"days": [...], "times": [...]}}`
with days `mon` to `sun` and 24-hour times (`"08:30"`, or with seconds), or
`{"once": "YYYY-MM-DDTHH:MM"}`.

**`time_zone`**: the IANA name the times are in, e.g. `"Europe/London"`.
Leave it out to use the system's.

## Playing

Playing an announcement starts one sender on the mcast server for each
group in its zones, all at once. A group reached through more than one zone
(or by both its phonebook name and its address) plays once.

The server lets one job send to a group at a time. For a group that's busy:

- `normal`: skip it (it shows as *busy*); the other groups still play.
- `high`: stop whatever is there and take the group over, unless it's
  another `high` or `emergency` announcement.
- `emergency`: take it over unless it's another `emergency` announcement.

Each repeat tries every group again, so a group that was busy can join in
later. A run is *done* if it played to at least one group, and *failed*
if it played to none.

## Schedules

Times are wall-clock times in the config's time zone, so 12:00 stays at
noon when the clocks change. On the day they change:

- A time the clocks skip (01:30 when they go forward from 01:00 to 02:00)
  plays at the same distance past the gap: 02:30.
- A time that happens twice (01:30 when they go back from 02:00 to 01:00)
  plays once, the first time.

A scheduled play starts on time if the panel is running. If it's up to 60 s
late (the panel was just starting) it still plays. Later than that it's
recorded in the history as *missed* rather than played late, so a restart
doesn't set off stale announcements. On starting, the panel looks back an hour
for times it missed while it wasn't running. If the mcast server can't be
reached at that moment, the play is recorded as *failed*; it isn't retried.

Changes made in the panel take effect straight away. Changes to the file made
by hand are picked up within 30 s.

## API

Everything is under `/api/intercom/v1`, as JSON, with `Authorization: Bearer
TOKEN` (or `?access_token=TOKEN`) when the panel has a token. Errors are
`{"error": "..."}`: `404` for something that isn't there, `409` for a
duplicate id, or a zone or announcement still in use, `422` for a config that wouldn't be valid,
`502` when the mcast server can't be reached.

| Route | |
|---|---|
| `GET /health` | `{"ok": true, "version"}` |
| `GET /backend` | The mcast server: `{"url", "connected", "error", "since_ms"}` |
| `GET /config` | `{"zones": [...], "announcements": [...]}` |
| `GET`, `POST /zones` | List, or add one (without `id`, one is made from `name`) |
| `GET`, `PUT`, `DELETE /zones/{id}` | `PUT` replaces it; the id can't change |
| `GET`, `POST /announcements`, `GET`, `PUT`, `DELETE /announcements/{id}` | The same |
| `GET`, `POST /schedules`, `GET`, `PUT`, `DELETE /schedules/{id}` | The same; without `id` or `name`, the id comes from the announcement |
| `GET /upcoming?n=10` | `{"time_zone", "now_ms", "fires": [{"at_ms", "local", "weekday", "schedule", "announcement", "name"}], "next": {SCHEDULE: at_ms or null}}` |
| `GET`, `PUT /settings` | `{"time_zone"}` (`null` for the system's); `GET` adds `effective_time_zone` |
| `POST /announcements/{id}/play` | Play it now; returns the run (`201`) |
| `GET /runs`, `GET /runs/{id}` | Runs playing and recently ended, newest first |
| `DELETE /runs/{id}` | Stop one; `DELETE /runs` stops them all |
| `GET /media`, `GET /phonebook` | From the mcast server, for choosing audio and groups |
| `GET /events` | Server-sent events: `run` (`{"run"}`), `config_changed`, `backend` (`{"status"}`), `lagged` |

A run:

```json
{"id": 3, "announcement": "lunch", "name": "Lunch bell", "priority": "normal",
 "trigger": {"type": "manual"}, "state": "playing", "round": 1, "rounds": 2,
 "started_ms": 1794022911066, "ended_ms": null, "error": null,
 "targets": [{"group": "zone-1", "address": "239.192.2.1:10000", "state": "playing",
              "job": 12, "error": null, "played": 0}]}
```

A scheduled run's `trigger` is `{"type": "schedule", "id", "at_ms"}`, with the
time it was due. Run `state` is `playing`, `waiting` (between repeats), `stopping`, `done`,
`stopped`, `failed` or `missed`. A target's is `starting`, `playing`, `done`, `busy`,
`preempted`, `stopped` or `failed`.

## Code

| File | |
|---|---|
| `model.rs` | Zones and announcements, validation, and the send request for a target |
| `store.rs` | Loading and saving `intercom.json` |
| `backend.rs` | The `Backend` trait (what the panel needs from a server) and `HttpBackend` |
| `runner.rs` | Plays announcements: fan-out, priorities, repeats, stop |
| `scheduler.rs` | When schedules are due (time zones, clock changes), and the thread that plays them |
| `cli.rs` | `mcast intercom list`, `check`, `play` |
| `server/` | `mcast intercom serve`: the API (`mod.rs`, `routes.rs`) and the web page (`web.rs`) |
| `assets/` | The web page: no framework and no build step, one module per tab (`panel.js`, `history.js`, `schedules.js`, `announcements.js`, `zones.js`); `api.js` is the only one that calls the server |

`cargo test -p mcast-intercom` runs the runner and the API against a fake
backend and, in `tests/server.rs`, against a real in-process `mcast serve`.

As in mcast-web, page text goes in with `h()`, never `innerHTML`, and styles
go in `app.css` or through `h(..., { style })`, because the Content-Security-Policy
forbids inline ones.
