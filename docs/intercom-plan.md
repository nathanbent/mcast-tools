# Intercom control panel: plan

Status: phases 1 and 2 built (`crates/mcast-intercom`: `mcast intercom list|check|play|serve`); phases 3-4 to come.

A small control panel on top of `mcast serve`: you define **zones** (sets of
speakers), **announcements** (audio + chimes + where it goes), and
**schedules**, then play announcements from big buttons or let the clock do it.

## Goal: separable from the core

The panel is a **client of the existing HTTP API**, the same way `mcast-tui`
is. It never links `mcast-control` or `mcast-server`; it has its own model
types for the few API shapes it reads. Everything it needs exists today:

| Need | Existing API |
|---|---|
| Play audio to a group | `POST /api/v1/senders` (file source, chime, end chime, voice mode, volume) |
| Stop it | `DELETE /api/v1/jobs/{id}` |
| Know when it finished | `GET /api/v1/events` (`job_ended`) |
| Pick audio | `GET/PUT /api/v1/media` |
| Pick speakers | `GET /api/v1/phonebook` (names resolve in `group`) |

So spinning it off later means moving one crate to a new repo and pointing it
at an `mcast serve` URL. No core changes are required for v1. One optional
core change is listed at the end.

## Shape

```
crates/mcast-intercom/          (new; depends on nothing in the workspace)
  src/
    lib.rs        run(config): start store, scheduler, HTTP server
    backend.rs    trait Backend + HttpBackend (talks to /api/v1, follows SSE)
    model.rs      Zone, Announcement, Schedule, Run (the panel's own data)
    store.rs      load/save intercom.json (atomic write)
    runner.rs     trigger an announcement: fan out to jobs, track a Run
    scheduler.rs  next-fire computation + the timer loop
    api.rs        its own JSON API (/api/intercom/v1)
  assets/         its own web UI (vanilla JS, no build step, like mcast-web)
  tests/          against an in-process mcast-server, like mcast-tui's tests
```

It runs as **its own process** (own port, own token) holding the mcast server URL
and token. `mcast intercom ...` is a thin subcommand in the `mcast` binary
(like `mcast tui`), so nobody has to install a second binary yet. On the Pi it
gets its own systemd unit next to `mcast.service`.

`Backend` is a trait so tests can use a fake, and so an in-process backend
could be added later if running two processes ever becomes a nuisance.

Dependencies: `axum`, `tokio`, `serde`, `serde_json` (already in the
workspace), `ureq` (already used by the TUI, run in `spawn_blocking`), and
**`jiff`** for time zones and DST in the scheduler.

## Data model

```jsonc
{
  "zones": [{
    "id": "z_upstairs", "name": "Upstairs",
    "targets": [
      // a phonebook name or an address, plus the send options that endpoint needs
      { "group": "zone-1", "encoding": "g711u" },
      { "group": "239.192.2.2:10000", "encoding": "l16", "volume_db": -3 }
    ]
  }],
  "announcements": [{
    "id": "a_lunch", "name": "Lunch bell",
    "audio": { "media": "lunch.wav" },      // or "chime_only"
    "chime": "default", "end_chime": null,
    "mode": "voice", "volume_db": 0,
    "repeat": 1, "repeat_gap_s": 2,
    "zones": ["z_upstairs", "z_downstairs"],
    "priority": "normal"                     // normal | high | emergency
  }],
  "schedules": [{
    "id": "s_lunch", "announcement": "a_lunch", "enabled": true,
    "when": { "weekly": { "days": ["mon","tue","wed","thu","fri"], "times": ["12:00"] } }
    // or { "once": "2026-12-24T16:30" }
  }],
  "time_zone": "Europe/London"
}
```

**Why codec lives on the zone target:** a phonebook entry is just an address;
different speakers need different encodings (G.711 ceiling speakers vs L16
amps). The target is the place that knows.

**Run** (in memory, plus a short history log): one firing of an announcement.
It holds the trigger (button / schedule id), one job id per target, and a
per-target state (playing / done / failed / skipped (busy)). A run is done when
every job has ended and any repeats are finished.

## Triggering (runner)

1. Resolve zones → de-duplicated list of targets (the same group reached by
   two zones is sent once).
2. For each target: `POST /senders` with the announcement's audio, chimes,
   mode, volume, plus the target's encoding/volume, `name` = announcement
   name (so it shows sensibly in SAP and in the existing web GUI).
3. Busy target (409): by priority:
   - `normal`: skip that target, mark it *busy* in the run.
   - `high`/`emergency`: stop the job using it (the 409 body names it), then retry.
     `emergency` also pre-empts other high-priority runs.
   - Later: an optional *queue* setting (wait for the group to free up).
4. Repeats: when all of a run's jobs end, wait `repeat_gap_s` and fire again.
   Done in the runner, not as a file loop, so every repeat gets its chime.
5. "Stop" on a run stops all of its jobs.

Starts happen in parallel so zones begin together. Exact sample alignment
across groups is out of scope.

## Scheduling

- A single loop: compute the next fire time across enabled schedules, sleep
  until then (or until the store changes), fire, repeat.
- Times are wall-clock in the configured `time_zone` (with `jiff`). DST: a time that
  doesn't exist that day fires at the next valid instant. A time that happens
  twice fires once.
- **Missed fires** (process was down, or the mcast server was unreachable):
  fire if late by less than a grace period (default 60 s), else log as missed.
  No catch-up storm after an outage.
- If the mcast server is down at fire time, the run records the failure. We
  don't retry silently: a bell 5 minutes late is usually worse than none.
- UI shows "next 10 upcoming fires" so schedules can be checked at a glance.

## Panel API (`/api/intercom/v1`)

```
GET/POST        /zones            GET/PUT/DELETE /zones/{id}
GET/POST        /announcements    GET/PUT/DELETE /announcements/{id}
POST            /announcements/{id}/play      -> Run
GET/POST        /schedules        GET/PUT/DELETE /schedules/{id}
GET             /upcoming?n=10
GET             /runs             DELETE /runs/{id}   (stop)
GET             /events           (SSE: run_started/updated/ended, config_changed)
GET             /backend          (is the mcast server reachable, its version)
```

Bearer token like the core server. Media, phonebook and SAP stay in the core
API; the panel proxies `GET` for media and the phonebook so the browser only talks to one origin.

## Web UI

Same style as `mcast-web` (no framework, `h()` not `innerHTML`, strict CSP),
but its own copy so it can leave with the crate:

- **Panel**: a grid of big buttons, one per announcement, coloured by priority;
  live state while playing; a Stop button; an "all stop".
- **Announcements / Zones** editors (media and phonebook pickers).
- **Schedule**: list with enable toggles, plus "upcoming".
- **History**: recent runs with per-zone outcome (played / busy / failed).

## Phases

1. **Core of it** (done): crate, `Backend` + `HttpBackend`, model + store, runner
   (fan-out, conflicts, stop, repeats). `mcast intercom play <name>` CLI for
   testing. Tests against an in-process `mcast-server`.
2. **Panel** (done): its HTTP API + web UI (panel grid, editors, history).
3. **Scheduler:** weekly + one-shot, time zone, grace, upcoming.
4. **Polish:** Pi systemd unit and install.sh option.

Not planned for now (possible later): live talk, queue-when-busy, skip dates
/ holidays / richer schedule rules, a TUI tab.

## Optional core change

- A free-form `tag` on send requests, echoed in `JobInfo`, so the panel can
  find its own jobs again after it restarts (today it would match on
  `name`). Small and useful to any client, so not panel-specific.

## Decisions

1. **Separate process**, with its own port and token, talking to `mcast serve` over HTTP.
2. **Pre-recorded only** (media files and chimes). Live talk may come later:
   live audio from the server's own input fits this design as a `capture` source.
   Talking from a browser's mic would need new core work.
3. **Conflicts:** `normal` skips busy groups; `high`/`emergency` take over
   from whatever is playing there, as described under Triggering.
4. **Schedules stay simple:** weekly (days + times) and one-off only.
