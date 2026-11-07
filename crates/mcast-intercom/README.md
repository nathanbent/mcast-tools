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

A page with a tab for each part:

| Tab | |
|---|---|
| **Panel** | A big button for each announcement, what's playing now (with Stop) and what's next. *All stop* in the header stops everything |
| **History** | Recent plays, and how each group fared |
| **Schedules** | When announcements play by themselves, under their **groups** (each with an on/off switch); what's coming up; *Pause all*; and **days off** (holidays, breaks) when no schedule plays. With **sites**, a bar picks all sites (a card for each: its time zone, what's next, pause) or one site (its schedules, pause switch and own days off) |
| **Announcements** | Each one in sections: General (name, audio), Chimes, Zones, Playback (repeats, priority) and Sound (processing, volume, lead-in) |
| **Sounds** | The mcast server's sound files, in folders: upload (or drop files on it), listen, rename or move, hide from the lists, delete. Renaming or moving a file updates the announcements that use it; one in use can't be deleted |
| **Zones** | Each zone's multicast groups, with the codec, volume, TOS, TTL and interface its speakers need |
| **Settings** | Announcement defaults (chimes, sound, and the codec for zone groups), the time zone, **sites**, which chimes the lists show, and the connection to the mcast server |

Sound files and phonebook names come from the mcast server, so its own web
page and TUI see the same files and folders.

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
  "defaults": {"chime": "ding-dong", "mode": "voice", "lead_in_ms": 300, "encoding": "g711u"},
  "days_off": [
    {"name": "Winter break", "from": "2026-12-21", "to": "2027-01-01"},
    {"name": "Inset day", "from": "2026-11-20"}
  ],
  "time_zone": "Europe/London"
}
```

**Zones.** `id` (letters, digits, `-`, `_`), `name`, and `targets`. Each target is a
multicast `group` (a phonebook name or an address) plus the send options
its speakers need: `encoding`, `volume_db` (added to the announcement's),
`tos`, `ttl`, `interface`. Leave `encoding` out to use the default codec
(`defaults.encoding`, else the mcast server's, L16), and any other option
out to use the server's default. `site`: the site it's at, if any.

**Announcements.** `id`, `name` and `zones` (zone ids), then:

| Field | Default | |
|---|---|---|
| `audio` | | `{"media": "NAME"}`, a file uploaded to the server, or `"chime_only"` |
| `chime`, `end_chime` | the defaults' | As in the API: `"default"` (the mcast server's: `rising` before, `falling` after), a built-in (`"ding-dong"`, `"westminster"`, `"church-bell"`, `"siren-wail"`, ...; `mcast audio chimes` lists them), or `{"file": "NAME"}`; or `"none"` for no chime even when the defaults have one. A `"chime_only"` announcement left with no chime at all plays the mcast server's default chime |
| `mode` | the defaults' | `voice`, `music` or `none` |
| `volume_db`, `lead_in_ms` | the defaults' | |
| `repeat` | `1` | Times to play it (up to 20); each repeat has its chimes |
| `repeat_gap_s` | `2` | Seconds between repeats |
| `priority` | `normal` | What happens to a group something else is already sending to (below) |

**Schedules.** `id`, `announcement` (its id), `enabled` (default `true`),
an optional `name`, and `when`: `{"weekly": {"days": [...], "times": [...]}}`
with days `mon` to `sun` and 24-hour times (`"08:30"`, or with seconds), or
`{"once": "YYYY-MM-DDTHH:MM"}`. Optionally `site` (a site's id: it plays only
to the announcement's zones at that site, in that site's time zone) and
`group` (a schedule group's id: it plays only while the group is on).

**`sites`**: places with their own zones and schedules, such as buildings or
campuses. Each has `id`, `name`, and optionally its own `time_zone` (else the
panel's), `days_off` (on top of the panel's) and `paused`. A schedule at a
site needs the announcement to have at least one zone there. Without sites,
everything is one place, as before.

**`schedule_groups`**: `[{"id", "name", "enabled"}]`, sets of schedules
switched on and off together: "Normal bells", "Exam week".

```json
"sites": [
  {"id": "north", "name": "North campus"},
  {"id": "south", "name": "South campus", "time_zone": "America/New_York",
   "days_off": [{"name": "Thanksgiving", "from": "2026-11-26", "to": "2026-11-27"}]}
],
"schedule_groups": [
  {"id": "normal", "name": "Normal bells", "enabled": true},
  {"id": "exams", "name": "Exam week", "enabled": false}
]
```

**`defaults`**: what announcements use for `chime`, `end_chime`, `mode`,
`volume_db` and `lead_in_ms` when they leave them out, so one chime can be
set for every announcement and changed in one place. An announcement's own
value always wins. Without `defaults`, a field left out means none, as
before. `encoding` is the codec for zone groups that don't name one
(`"g711u"`, `"g722"`, ...); without it they get the mcast server's, L16.

**`days_off`**: dates no schedule plays on, each `{"from": "YYYY-MM-DD"}`,
with `"to"` (the last day, included) for more than one day and an optional
`"name"`. Playing from the panel or the command line still works.

**`hidden_media`**: media files and folders (`"bells/old"`) the panel leaves
out of its lists; everything in a hidden folder is hidden. Announcements that
use one still play it.

**`schedules_paused`**: `true` stops every schedule playing, without
turning each one off.

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

A schedule plays when it's on, its group (if any) is on, its site (if any)
isn't paused, schedules aren't all paused, and the day isn't a day off (the
panel's, or its site's).

Times are wall-clock times in the schedule's site's time zone (else the
panel's), so 12:00 stays at noon when the clocks change. On the day they
change:

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

No schedule plays on a day off, or while schedules are paused; those times
aren't recorded as missed either. A run of days off can be as long as a
school holiday.

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
| `GET /config` | The whole config, as in `intercom.json` |
| `GET`, `POST /zones` | List, or add one (without `id`, one is made from `name`) |
| `GET`, `PUT`, `DELETE /zones/{id}` | `PUT` replaces it; the id can't change |
| `GET`, `POST /announcements`, `GET`, `PUT`, `DELETE /announcements/{id}` | The same |
| `GET`, `POST /schedules`, `GET`, `PUT`, `DELETE /schedules/{id}` | The same; without `id` or `name`, the id comes from the announcement |
| `GET`, `POST /sites`, `GET`, `PUT`, `DELETE /sites/{id}` | The same; a site with zones or schedules can't be deleted (`409`) |
| `GET`, `POST /schedule-groups`, `GET`, `PUT`, `DELETE /schedule-groups/{id}` | The same; switch one with `PUT` and `"enabled"`. One with schedules can't be deleted |
| `GET /upcoming?n=10&site=ID` | `{"time_zone", "paused", "now_ms", "fires": [{"at_ms", "local", "weekday", "schedule", "announcement", "name", "site"}], "next": {SCHEDULE: at_ms or null}, "sites": {ID: {"time_zone", "paused"}}}`. `site` limits it to one site's schedules (empty for those with none). `local` is in the schedule's own time zone. `fires` leaves out days off and schedules that can't play; `next` leaves out days off only |
| `GET`, `PUT /settings` | `{"time_zone", "defaults", "schedules_paused", "days_off"}`, as in the config (`time_zone` `null` for the system's). `PUT` changes the fields given and leaves the rest; `defaults` and `days_off` are replaced whole. `GET` adds `effective_time_zone` |
| `POST /announcements/{id}/play` | Play it now; returns the run (`201`) |
| `GET /runs`, `GET /runs/{id}` | Runs playing and recently ended, newest first |
| `DELETE /runs/{id}` | Stop one; `DELETE /runs` stops them all |
| `GET /media`, `GET /phonebook` | From the mcast server, for choosing audio and groups: media names are paths (`bells/lunch.wav`) |
| `GET /library` | The Sounds tab: `{"files": [{"name", "bytes", "modified_ms", "hidden", "used_by"}], "folders": [{"name", "hidden"}]}`; `used_by` names the announcements (and "the defaults") that use a file |
| `GET`, `PUT`, `DELETE /media/{path}` | Listen to, upload (raw body) or delete a file on the mcast server. `DELETE` is refused (`409`) while something uses it |
| `POST /media-folders`, `DELETE /media-folders/{path}` | `{"name"}`: make a folder; delete an empty one |
| `POST /media-move` | `{"from", "to"}`: rename or move a file or folder, then point what used it at the new name. Returns `{"from", "to", "folder", "updated"}` |
| `PUT /media-hidden` | `{"name", "hidden"}`: hide a file or folder from the lists, or show it again |
| `GET /chimes`, `PUT /chimes`, `GET /chimes/{name}/preview` | The mcast server's built-in chimes: list them, hide some from the lists (`{"hidden": [...]}`, kept on the mcast server), and a WAV to listen to |
| `GET /events` | Server-sent events: `run` (`{"run"}`), `config_changed`, `backend` (`{"status"}`), `media_changed`, `lagged` |

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
| `model.rs` | Zones, announcements, defaults and days off, validation, and the send request for a target |
| `store.rs` | Loading and saving `intercom.json` |
| `backend.rs` | The `Backend` trait (what the panel needs from a server) and `HttpBackend` |
| `runner.rs` | Plays announcements: fan-out, priorities, repeats, stop |
| `scheduler.rs` | When schedules are due (time zones, clock changes), and the thread that plays them |
| `cli.rs` | `mcast intercom list`, `check`, `play` |
| `server/` | `mcast intercom serve`: the API (`mod.rs`, `routes.rs`) and the web page (`web.rs`) |
| `assets/` | The web page: no framework and no build step, one module per tab (`panel.js`, `history.js`, `schedules.js`, `announcements.js`, `zones.js`, `sounds.js`, `settings.js`), and `chimes.js` for the chime fields and lists; `api.js` is the only one that calls the server |

`cargo test -p mcast-intercom` runs the runner and the API against a fake
backend and, in `tests/server.rs`, against a real in-process `mcast serve`.

As in mcast-web, page text goes in with `h()`, never `innerHTML`, and styles
go in `app.css` or through `h(..., { style })`, because the Content-Security-Policy
forbids inline ones.
