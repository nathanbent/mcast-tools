# Intercom: where things stand (handoff)

Last updated 2026-11-08, at the end of a third session. The second session
checked the first one's work, built everything left on the roadmap plus
follow-ups, merged it all into `main`, and bumped the version to 0.5.5
(#48). The third built **text pages in playlists** (#49, below), merged it,
and released **v0.5.5**. Read this first,
then `intercom-roadmap.md` (the plan and what each item became) and
`intercom-plan.md` (how the panel is built).

## Where things stand

- **Everything is on `main` and released as v0.5.5.** Every item on the roadmap (v0.5.5 and v0.6.0) is built, plus follow-ups:
  - last-heard times in the Zones list;
  - alerts when a speaker goes offline between plays;
  - per-site webhooks and monitor;
  - a webhook delivery log;
  - text pages in playlists, spoken and shown on displays (nathanbent/mcast-tools#49).
- **v0.5.5 is released:** https://github.com/nathanbent/mcast-tools/releases/tag/v0.5.5, tag `v0.5.5` on `7f3c8c7` (the merge of #49), with binaries for Linux (x86_64, aarch64), macOS (Intel, Apple Silicon) and Windows. The owner chose to ship it all as 0.5.5; there's no separate 0.6.0. Pis upgrade with `sudo mcast update`.
- **It was released before the hardware checks** below (What's next, 1). Their results decide whether a 0.5.6 is needed.
- **No PRs are open.** New work branches from `main`; the next release needs a version bump first.

The work came as six stacked PRs, merged in order with merge commits:

| PR | Branch | What |
|---|---|---|
| nathanbent/mcast-tools#38 | `claude/intelligent-hypatia-awklrl` | v0.5.5: tokens, triggers, webhooks, sounds, files, playlists, history, monitor, Panel organizing, kiosks |
| nathanbent/mcast-tools#43 | `claude/vibrant-knuth-34bpdf` | Delivery confirmation: which speakers heard each play, and which are offline between plays (item 11) |
| nathanbent/mcast-tools#44 | `claude/intercom-backup-restore` | Backup and restore, with the sounds (item 13) |
| nathanbent/mcast-tools#45 | `claude/intercom-gpio` | Buttons and door contacts on GPIO pins (item 12) |
| nathanbent/mcast-tools#46 | `claude/intercom-site-webhooks` | Per site: webhooks limited to sites, and the Monitor tab for one site |
| nathanbent/mcast-tools#47 | `claude/intercom-webhook-log` | A delivery log for each webhook |

- **Before merging,** CI was green on every PR's latest commit, with no merge conflicts and no review comments. Each was merged once the one below it had landed and it had been pointed at `main`.
- **Their branches are still on GitHub** (the repo doesn't delete branches on merge). They can be deleted; nothing depends on them.
- **Watches and check-ins:** the session that did this was watching the PRs until they merged, and has no check-ins left.

## Checked

- **At the start of the session,** on #38's head (`6908d0f`): fmt, clippy (`-D warnings`) and `cargo test --workspace` all passed (271 tests), and GitHub CI was green. Every item listed below is in the code, with no TODOs left.
- **On `main` after the merges** (`c1c6ec7`), the same checks pass: 306 tests. They pass again with the version bump.
- **Docs** that had fallen behind the code were fixed in #38: the roadmap's status, item 9 (the Panel is arrangeable too), item 11 (the core didn't expose listener reports yet), and `intercom-tokens.md` (history is persisted).

## Text pages (third session)

The owner answered the open question: text pages are **both** spoken and
shown, chosen per page (both by default), with Piper for speech (eSpeak NG
as the fallback), shown **in step** with the audio, on **both** kinds of
display.

- **Core (`mcast serve`), general, not panel-specific:**
  - `mcast_audio::tts`: Piper (if `piper` is on the `PATH` with a voice in
    `--tts-voices`, default `/usr/local/share/mcast/voices`), else eSpeak NG,
    or `--tts-command` (text on stdin, WAV on stdout). WAVs are cached by
    engine, voice and text (`--tts-cache`, `MCAST_TTS_CACHE`; owner-only
    directory, 500 files kept). `mcast serve` says at start which it found.
  - A `files` source's items can be `{"say": TEXT, "voice"}`; the server
    speaks them before starting the sender. `GET /api/v1/voices`.
  - Senders report `stats.item` (the item playing), with an immediate
    `job_updated` as each starts. `FileList` reports a file when it's first
    read, after the gap before it; the sender reads a packet at a time, so
    this is within a packet of when it's sent.
  - Documented in `docs/api.md` (*Text to speech*).
- **Panel:**
  - `PlaylistItem::Text { text, speak, show, voice }`. Spoken pages go to
    the server as `say` items; pages only shown don't go in the stream.
    `Playlist::texts_by_item` maps each item heard to the pages to show as
    it starts: a page only shown waits for the next item heard (or goes
    with the last). A playlist needs something heard to be played.
  - Zones have `displays` (groups, up to 20).
  - The runner follows `Feed::JobItem` (from `job_started`/`job_updated`
    events with `stats.item`); items that arrive before the job is ours wait
    in `early_items`. It records `run.shown` once per (part, round, item)
    and emits `Change::Shown`.
  - `crate::displays`: the board (`GET /display?zones=`, `display` event)
    keeps a page up while its run plays and `HOLD` (30 s) after; a stopped
    run's pages come down at once. A worker thread sends pages to display
    groups on a text channel per group (`POST /texts`, named "Intercom",
    SAP on), reopened if it went away, closed after 5 idle minutes.
  - `GET /voices` passes on the server's. Limited tokens see only their
    categories' pages.
  - Page: text pages in the Playlists editor (Speak, Show on displays, a
    voice), Text displays on zones, `?display=1&zones=a,b` (a full-screen
    sign with a clock in between; Settings → Panel builds the link), and
    "Shown: ..." on run cards.
- **Pi:** `TEXT_TO_SPEECH=espeak|piper|none` (espeak by default) and
  `PIPER_VOICE` in `pi.conf`. Piper is the 2023.11.14-2 release binary in
  `/opt/piper` with a wrapper in `/usr/local/bin`, its voice from Hugging
  Face. `mcast.env` sets `MCAST_TTS_CACHE=/var/lib/mcast/tts-cache`.
- **Tests:** TTS unit tests (a fake command; eSpeak NG as an ignored test),
  item reporting in the sender, an API test in `mcast-server` (speaking,
  caching, items in order, an engine that fails), panel unit and API
  tests, and an end-to-end test (`tests/server.rs`) with a real
  `mcast serve`: speech by command, items in step, and the text arriving
  on the display group through a real text channel.
- **Checked by hand** with the real binaries and real eSpeak NG: the
  editor, the zone form, the display page (text up 1.3 s after play, as the
  spoken page started) and `mcast text recv` showing "Intercom: ...".
- **Not checked:** Piper with a real voice. Hugging Face was blocked from
  that container. `piper` 1.8's flags were checked (`-m`, `-f`, text on
  stdin), and the C++ release takes the same. **Try `TEXT_TO_SPEECH=piper`
  on a Pi.**

## What's built

### In #38 (v0.5.5)

The v0.5.5 items, plus requests that came up along the way:

1. **Scoped access tokens** (`docs/intercom-tokens.md`):
   - Roles admin, operator, trigger and media.
   - Kept hashed in `tokens.json`, with expiry and use counts.
   - `mcast intercom token add|list|revoke`.
   - Operator tokens can be limited to categories; trigger tokens to announcements.
2. **Trigger API and webhooks** (`docs/intercom-triggers.md`): `/trigger/{name}` by GET or POST, `/stop-all`, and signed outbound webhooks in `webhooks.json`.
3. **Remote sounds:** uploads checked by `mcast serve` (`?audio=1`), each file's length (`/media?audio=1`), and recording from the browser.
4. **File manager:** tags, search, multi-select bulk actions, drag a file onto a folder to move it, and sorting.
5. **Theme switch:** System, Light or Dark per browser, on both pages, with no flash on load.
6. **Persistent history:**
   - `history/YYYY-MM-DD.jsonl`, kept 90 days by default.
   - The History tab, with filters and CSV.
   - Paging uses a `started_ms:id` mark.
7. **Stats strip** on the Panel (`/stats`).
8. **Playlists and parts:**
   - A core `files` source plays a list of files as one stream.
   - Playlists tab; an announcement's audio can be a playlist.
   - Parts send other audio to other zones in the same run.
9. **Arrangeable layout:** admins order sections (Settings), and customize the Panel in place: groups made, renamed, colored and deleted, buttons dragged between them, saved at once (`PUT /panel-layout`) for every screen.
10. **Organizing the Panel:**
    - Categories, and tags as filter chips.
    - Settings → Panel: button size, tiles or a list, what's shown, confirm emergencies.
    - Colors from a fixed palette, icons, ask before playing.
    - Categories fold per screen.
11. **Per-screen views:** category-limited operator tokens, enforced by the server, and kiosk mode (`?kiosk=1`, plus `categories=`, `tag=`, `size=`, `mode=`) with a link builder.
12. **Monitor tab** (v0.6 item 10):
    - `/monitor?days=N`: per day, announcement, zone and schedule timing, and what started plays.
    - Server uptime from `history/connection.jsonl`.
    - Status-colored chart with a table view.
13. **Review and polish pass:**
    - Nine fixes, including `/events` filtering for limited tokens and zones used only by parts.
    - A More menu in the header for Playlists, Sounds, Zones and Settings.
    - Theme icons.
    - `docs/intercom-plan.md` rewritten as an overview.

### In #43: delivery confirmation (roadmap item 11)

"Probes" means speakers running `mcast receive --name`; the owner decided this.

- **Core (backwards compatible):**
  - `GET /api/v1/monitor` and the `monitor` event carry `listeners`: each receiver heard on a watched group's RTCP port, with the SSRC it hears (`about`).
  - Audio send jobs report their `ssrc`, which ties a listener to a page exactly. No `tag` on send requests was needed.
  - Documented in `docs/api.md`.
- **Panel:**
  - A zone's `listeners` names the speakers expected to hear it. The Zones tab offers the names heard now (`GET /listeners`).
  - The runner checks every play to such zones through `/monitor`: once a second while it plays, and for up to 12 s after (`LISTEN_GRACE`), stopping early once everyone expected was heard.
  - The run gets `listeners: {checking, expected, heard [{name, loss_percent}], silent, error}`.
  - History writes a checked run once the check ends.
  - A `listener_silent` webhook, a toast, "Heard by 1 of 2 · silent: Office" on run cards, and a Speakers table on the Monitor tab.
  - If the monitor is off or unreachable, `listeners.error` says so and nobody is called silent.
- **The Zones list** shows each expected listener with a dot: reporting, offline (and since when), or checking. It also says when the listener last heard a page, from the history over 30 days.
- **Speakers going offline between plays** (`crate::presence`):
  - The panel asks the monitor every 10 s (`POLL`).
  - A speaker missing for a minute (`OFFLINE_AFTER`) is offline: a `listener_offline` webhook, a toast, and "Speakers: Office offline" on the Panel's stats strip.
  - When it's back: `listener_online` and a toast.
  - The state is at `GET /speakers` and in the `speakers` event.
  - Nothing is called offline while the monitor can't be asked.
- **Tests:**
  - Unit and API tests against the fake.
  - An end-to-end test (`tests/server.rs`) with a real `mcast serve` and a real named receiver.
  - Checked in a browser with the real binaries: the alerts with real receivers (one never running, one stopped and restarted) and a local webhook receiver.

### In #46: per-site webhooks and monitor (follow-ups)

- **The setting:** a webhook's `sites` (Settings → Other systems, ticked per site) limits it to plays that reached a zone at one of those sites, and to speakers expected by such zones.
- **No sites set:** the hook gets everything, as before. Plays to zones at no site go only to such hooks.
- **Always sent:** `backend_down` and `backend_up`, which are about the panel itself.
- **What runs and speakers record:** each run has `sites` (its zones' sites, or the schedule's site), and each speaker has `sites`.
- **Validation:** unknown sites are refused when a hook is saved. A filter on a site deleted later is kept, and shows as "(deleted)".
- **Tests:** a unit test of the filter, and an API test with three receivers (North, South, all) and plays to one site and to both. The form was checked in a browser.
- **The Monitor tab for one site:** a site picker (remembered per screen), backed by `GET /monitor?site=ID`.
  - It keeps plays at that site: by a run's `sites`, or for older runs, by the zones its groups are in now.
  - It counts only that site's groups (a play to two sites counts once in each), and lists only its zones and expected speakers.
  - The mcast server's uptime stays panel-wide.
  - Tested by unit and API tests, and in a browser with real plays.

### In #47: webhook delivery log (a follow-up)

- **What's kept:** each hook's last 50 deliveries (`LOG_PER_HOOK`): its number, event, what it was about, when, the result (status and error), attempts and time taken.
- **Where:** `GET /webhooks/{id}/deliveries` (admin), and a *Log* button on each hook in Settings → Other systems.
- **The file:** `webhook-log.jsonl` beside `webhooks.json`, owner-only, so the log outlives a restart. It's cut back past 5000 lines, and a deleted hook's lines go with it. No bodies, so no secrets.
- **Tests:** a unit test (the 50 cap, the file's permissions, reading it back, cutting it back, forgetting a hook) and an API test (a run and a test delivery logged with what they were about, the log after a restart, and gone with the hook). Checked in a browser with a working receiver and a dead one (failed, 2 attempts, 3000 ms).
- **Order (later, as the owner asked):** deliveries are sent and logged on their own threads, so they can finish out of order (a slow receiver, a retry). The log keeps them in the order sent, by `at_ms` then the delivery number, in memory, read back from the file and when it's rewritten. Tested with a receiver that answers `run_started` slowly, so `run_ended` finishes first.

### In #44: backup and restore (roadmap item 13)

- **What a backup holds:** the owner chose `intercom.json` alone, or a `.tar.gz` with every sound on the mcast server too (`mcast-intercom-backup.json`, `intercom.json`, `media/PATH`). Never tokens, webhooks or history.
- **Restoring:**
  - It says first what it adds, changes and removes (by id, for zones, announcements, schedules, playlists, categories, sites, schedule groups and inputs), which other settings change, which sounds it uploads, and which would be missing.
  - Then it uploads the sounds the server lacks or has at another size, keeps `intercom.json.bak`, and writes the config.
  - A sound the server refuses leaves the config unchanged.
- **Where:** Settings → Backup and restore; `mcast intercom backup [--sounds] [-o FILE]` and `restore FILE [--dry-run]`; `GET /backup[?sounds=1]` and `POST /restore[?dry_run=1]` (admin).
- **`crate::backup`** holds the archive, `plan` and `apply` logic, shared by the API and the CLI. Archives are checked: only the expected file names, no `..` or hidden paths, at most 1 GiB unpacked and 10,000 files.
- **Dependency:** adds `tar` (with `filetime`); `flate2` was already in the tree.
- **Tests:** unit and API tests. The CLI was checked against a real `mcast serve` with a real WAV, and the page in a browser.

### In #45: GPIO buttons and contacts (roadmap item 12)

- **How pins are read:** the owner chose the `gpiocdev` crate. It's Linux only (a target-gated dependency) and adds `gpiocdev`, `gpiocdev-uapi` and `ioctl-sys`.
- **Config:** `inputs` in `intercom.json`: `line`, `chip` (default `gpiochip0`), `active_low` (default true), `bias` (pull-up), `debounce_ms` (50, done by the kernel), `announcement`, `while_held` and `enabled`.
- **Behaviour:**
  - A press plays the announcement at its own priority. Re-pressing while it plays does nothing.
  - `while_held` stops it on release.
  - A contact closed at start-up doesn't page.
  - Runs record `trigger: {"type": "input", "id", "by"}`.
- **`crate::inputs`:** a `Pins` trait (`gpiocdev` on Linux, a "needs Linux" stub elsewhere, `FakePins` for tests). `Inputs` follows the config: poked on edits, re-read every 5 s, and a pin that won't open is retried every 30 s.
- **API (admin):** `/inputs`, `/input-status`, `/inputs/{id}/test`, and an `inputs` event.
- **Page:** Settings → Buttons and contacts: each pin's state, with its header pin on a Pi, and a Test button.
- **Pi:** `install.sh` adds `mcast` to `gpio`; `pi/README.md` has a wiring section.
- **Not checked on real hardware:** this machine had no GPIO. The `gpiocdev` code ran and reported the missing chip, and everything after the pin is tested with fake pins. `install.sh` passed `shellcheck` in #45's CI. **Press a real button on a Pi** (not done before v0.5.5 went out).

## What's next

1. **Try the hardware** with v0.5.5 (`sudo mcast update`, then re-run `install.sh`). Nothing here has run on real hardware yet:
   - **A GPIO button on a Pi** (#45). Wire a switch from GPIO17 (header pin 11) to ground, add it in Settings → Buttons and contacts, and press it. The state should change and the announcement play.
   - **Delivery confirmation and offline alerts** with real speakers on the network (#43). These were tested only with receivers on one machine, over loopback. Run `mcast receive --name ROOM` on two or three speakers and list them in a zone. A play should say who heard it, and unplugging one should bring an offline alert after about a minute.
   - **Text pages on a Pi** with `TEXT_TO_SPEECH=piper`: the voice downloads, a spoken page sounds right, and `mcast text recv -g GROUP` on another box shows it.
2. **Fix anything the hardware turns up,** then bump to 0.5.6 and release it the same way (Release workflow from `main`, tag `v0.5.6`).
3. **Smaller follow-ups:**
   - "Arrange this page" on more tabs (Schedules), if the owner wants it.
   - (The flaky `webhooks_keep_a_log_of_their_deliveries` test was fixed in nathanbent/mcast-tools#50: it now waits for each delivery to be logged before setting off the next.)

## Questions still open for the owner

1. **Delivery confirmation:**
   - How much loss still counts as "heard it"? For now any report naming the stream counts, and the worst loss is recorded so a threshold can be added.
2. **Arrange this page:** roll it out to more tabs?
3. **Text pages, choices to confirm:**
   - A page stays on the display page for 30 s after its run ends (`displays::HOLD`).
   - A page only shown appears with the next item heard, not on its own timer.
   - The default Piper voice on a Pi is `en_US-lessac-medium`.
4. **GPIO choices to confirm (#45):**
   - An input plays at its announcement's priority, with no per-input priority.
   - Re-pressing while it plays is ignored, not restarted.
5. **Offline timing to confirm (#43):** a minute without a report, checked every 10 s, so an alert comes 60 to 70 s after a speaker's last report. Fine for a speaker that was unplugged; shorter would mean false alarms from a briefly busy network.

## How to work on it

- **Git:** branch from `main` and open a PR against it. The history uses merge commits ("Merge pull request #N"). CI (`.github/workflows/ci.yml`) runs fmt, clippy, the tests and `shellcheck` on `pi/`. A CI failure in the "restore cache" step, before anything is built, has been a GitHub cache hiccup; one re-run cleared it.
- **Releasing:** bump `version` in `Cargo.toml` (the lockfile follows on the next build), merge, then run the Release workflow from `main` with the matching tag.
- **Build:** `cargo build -p mcast`. Needs `libasound2-dev`, `libopus-dev` and `pkg-config` (apt).
- **Checks** (CI runs the same): `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- **MSRV is 1.75** (`rust-version` in `Cargo.toml`): avoid newer std APIs (for example, use `map_or(true, …)`, not `is_none_or`). CI only builds on stable. Some dependencies already in `main`'s lockfile need more than 1.75 (`thiserror` 2.0.21 needs 1.77), so the declared MSRV isn't really checked.
- **Run it by hand:**
  - `mcast serve --media-dir DIR --no-monitor`
  - `mcast intercom serve --config DIR/intercom.json --no-tls --web-dir crates/mcast-intercom/assets`. `--web-dir` serves the page from disk, so a reload picks up JS and CSS edits.
  - Add `--panel-token X` to test tokens and roles.
- **Browser checks:** Playwright is preinstalled (`NODE_PATH=$(npm root -g) node script.js`). Chromium's fake mic is `--use-fake-ui-for-media-stream --use-fake-device-for-media-stream`.
- **Conventions in the intercom crate:**
  - Every API route needs a line in `server/auth.rs` `access()`, and in the `ROUTES` list in `server/tests.rs`; a test fails otherwise.
  - Owner-only files are written with `store::save_private`.
  - Page JS builds DOM with `h()`, never `innerHTML`. The strict CSP allows no inline scripts or style attributes; set styles through `el.style`.
  - Shared page helpers:
    - `layout.js`: `arrangeable`, `sortableList`, `arrangeDialog`.
    - `drag.js`: dragging into order with pointer events (mouse, touch with a long press or a handle, pen), used by `sortableList` and the Panel's Customize mode.
    - `customize.js`: the Panel's Customize mode.
    - `store.js`: `onRunEnded` (as a run ends), `onRunSettled` (once it's in the history, after any listener check), `onSpeakersChanged`, `panelAnnouncements`, `can`, `TABS`.
    - `editor.js`: `mountEditor`, `mediaFilter`, `colorSelect`.
- **Test helpers:**
  - `presence::Presence::start` takes the poll and offline times, so tests can shorten them.
  - `crate::fake::Fake` is a fake mcast server. Its `listeners` field sets what `/monitor` reports, and its jobs' `ssrc` is `0x5000_0000 + job id`.
  - `Runner::with_listen_timing` shortens the listener check's waits.
  - `inputs::FakePins` with `AppState::with_pins` presses GPIO pins.
- **Running a named receiver without a sound card:** `mcast audio recv -g GROUP --wav FILE -n Lobby`, with `mcast serve --watch GROUP` so its monitor hears it.
- **Gotcha:** `pkill -f 'mcast serve'` inside a script can kill the script itself. Use a pattern like `'[t]arget/debug/mcast serve'`.
