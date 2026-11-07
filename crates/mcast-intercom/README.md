# mcast-intercom

An intercom control panel for `mcast serve`. You set up **zones** (sets of speakers)
and **announcements** (an audio file or a chime, which zones it goes to, how
many times, at what priority), then play them.

It is a client of the mcast HTTP API and nothing else: it doesn't use any
other crate in this workspace, so it can be moved out on its own. See
[`docs/intercom-plan.md`](../../docs/intercom-plan.md) for the plan: a web
panel and schedules come next.

## Use

`mcast serve` must be running. The intercom talks to it at `--url`
(`$MCAST_URL`, default `http://127.0.0.1:8650`) with `--token` (`$MCAST_TOKEN`).

```bash
mcast intercom list            # zones and announcements
mcast intercom check           # the config, and its media files and phonebook names on the server
mcast intercom play lunch      # by id or name; Ctrl-C stops it
```

## Config

`intercom.json` in the mcast config directory (`~/.config/mcast`), or the
file named by `--config` or `$MCAST_INTERCOM`. Edit it by hand for now;
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
  ]
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

## Code

| File | |
|---|---|
| `model.rs` | Zones and announcements, validation, and the send request for a target |
| `store.rs` | Loading and saving `intercom.json` |
| `backend.rs` | The `Backend` trait (what the panel needs from a server) and `HttpBackend` |
| `runner.rs` | Plays announcements: fan-out, priorities, repeats, stop |
| `cli.rs` | `mcast intercom list`, `check`, `play` |

`cargo test -p mcast-intercom` runs the runner against a fake backend and,
in `tests/server.rs`, against a real in-process `mcast serve`.
