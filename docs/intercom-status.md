# Intercom: where things stand (handoff)

Last updated 2026-11-08, late in a second session. That session checked the
first one's work, then built everything left on the v0.6.0 roadmap, plus two
follow-ups: last-heard times in the Zones list, and alerts when a speaker
goes offline between plays. Read this first, then `intercom-roadmap.md`, which has the full plan
and what each item became.

## Where things stand

Every item on the roadmap for v0.5.5 and v0.6.0 is built. Nothing is merged
yet: the work is four PRs, each stacked on the one before so its diff shows
only its own feature.

| PR | Branch | What | Based on |
|---|---|---|---|
| nathanbent/mcast-tools#38 | `claude/intelligent-hypatia-awklrl` | v0.5.5: tokens, triggers, webhooks, sounds, files, playlists, history, monitor, Panel organizing, kiosks | `main` |
| nathanbent/mcast-tools#43 | `claude/vibrant-knuth-34bpdf` | Delivery confirmation: which speakers heard each play, and which are offline between plays (item 11) | #38 |
| nathanbent/mcast-tools#44 | `claude/intercom-backup-restore` | Backup and restore, with the sounds (item 13) | #43 |
| nathanbent/mcast-tools#45 | `claude/intercom-gpio` | Buttons and door contacts on GPIO pins (item 12) | #44 |
| (this one) | `claude/intercom-site-webhooks` | Webhooks limited to sites (a follow-up) | #45 |

- **State:** all four are open, with no merge conflicts and no review comments, waiting on the owner.
  - CI is green on the latest commits of #38 (`3f6402b`), #43 (`723c611`) and #44 (`6a6ee6e`).
  - On #45 (`dadf4c0`), `shellcheck` passed and the Rust job was still running when this was written. The same checks pass locally there.
- **Changes low in the stack are merged up:** a fix or follow-up on #43 is merged into #44's branch, then #44's into #45's, so the PRs stay consistent. Conflicts so far were neighbouring additions (routes, events, tests, docs), resolved by keeping both. After one, check every `#[tokio::test]` attribute survived: clippy's dead-code error caught one that didn't.
- **Merging:** merge them in order (#38, then #43, #44, #45). When one merges and its branch is deleted, GitHub retargets the next PR to the merged one's base. If the branch is kept, change the next PR's base by hand.
- **`main`** is at v0.5.4 and was last merged into #38 at that version. If `main` moves, merge it into #38's branch and carry it up the stack (merge each branch into the next). Conflicts so far were small, in `announcements.js` and `talk.js`.
- **After merging:** release v0.5.5 (#38), then v0.6.0 (the rest), or all of it as v0.6.0.
- **A Claude session was watching all four PRs** (reviews, CI) and fixing what came up. A new session won't be; ask it to subscribe if wanted.

## Checked

- **At the start of the session,** on #38's head (`6908d0f`): fmt, clippy (`-D warnings`) and `cargo test --workspace` all passed (271 tests), and GitHub CI was green. Every item listed below is in the code, with no TODOs left.
- **At the top of the stack** (#45, `dadf4c0`), the same checks pass: 300 tests. #43's branch alone has 287.
- **Docs** that had fallen behind the code were fixed in #38: the roadmap's status, item 9 (the Panel is arrangeable too), item 11 (the core didn't expose listener reports yet), and `intercom-tokens.md` (history is persisted).

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
9. **Arrangeable layout:** admins order sections (Settings, and the Panel's categories and buttons), shared by every screen.
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

### Webhooks limited to sites (a follow-up, the PR after #45)

- **The setting:** a webhook's `sites` (Settings → Other systems, ticked per site) limits it to plays that reached a zone at one of those sites, and to speakers expected by such zones.
- **No sites set:** the hook gets everything, as before. Plays to zones at no site go only to such hooks.
- **Always sent:** `backend_down` and `backend_up`, which are about the panel itself.
- **What runs and speakers record:** each run has `sites` (its zones' sites, or the schedule's site), and each speaker has `sites`.
- **Validation:** unknown sites are refused when a hook is saved. A filter on a site deleted later is kept, and shows as "(deleted)".
- **Tests:** a unit test of the filter, and an API test with three receivers (North, South, all) and plays to one site and to both. The form was checked in a browser.

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
- **Not checked on real hardware:** this machine had no GPIO. The `gpiocdev` code ran and reported the missing chip, and everything after the pin is tested with fake pins. `install.sh` passed `shellcheck` in #45's CI. **Press a real button on a Pi before releasing.**

## What's next

1. **The owner reviews and merges** #38, #43, #44 and #45, in that order.
2. **Try the hardware:**
   - A button on a Pi (#45).
   - Delivery confirmation and offline alerts with real speakers on the network (#43). They were tested with receivers on one machine, over loopback.
3. **Release.**
4. **Text pages in playlists:**
   - `PlaylistItem` (`model.rs`) has one variant, `Sound`, and is ready for another.
   - Blocked on the owner: displays through `mcast text`, read aloud (TTS, listed as out of scope), or both.
5. **Smaller follow-ups:**
   - A webhook delivery log (the last N deliveries per hook).
   - Monitor charts per site.
   - "Arrange this page" on more tabs (Schedules), if the owner wants it.

## Questions still open for the owner

1. **Delivery confirmation:**
   - How much loss still counts as "heard it"? For now any report naming the stream counts, and the worst loss is recorded so a threshold can be added.
2. **Text pages in playlists:** displays, TTS, or both?
3. **Arrange this page:** roll it out to more tabs?
4. **GPIO choices to confirm (#45):**
   - An input plays at its announcement's priority, with no per-input priority.
   - Re-pressing while it plays is ignored, not restarted.
5. **Offline timing to confirm (#43):** a minute without a report, checked every 10 s, so an alert comes 60 to 70 s after a speaker's last report. Fine for a speaker that was unplugged; shorter would mean false alarms from a briefly busy network.

## How to work on it

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
    - `store.js`: `onRunEnded` (as a run ends), `onRunSettled` (once it's in the history, after any listener check), `onSpeakersChanged`, `panelAnnouncements`, `can`, `TABS`.
    - `editor.js`: `mountEditor`, `mediaFilter`, `colorSelect`.
- **Test helpers:**
  - `presence::Presence::start` takes the poll and offline times, so tests can shorten them.
  - `crate::fake::Fake` is a fake mcast server. Its `listeners` field sets what `/monitor` reports, and its jobs' `ssrc` is `0x5000_0000 + job id`.
  - `Runner::with_listen_timing` shortens the listener check's waits.
  - `inputs::FakePins` with `AppState::with_pins` presses GPIO pins.
- **Running a named receiver without a sound card:** `mcast audio recv -g GROUP --wav FILE -n Lobby`, with `mcast serve --watch GROUP` so its monitor hears it.
- **Gotcha:** `pkill -f 'mcast serve'` inside a script can kill the script itself. Use a pattern like `'[t]arget/debug/mcast serve'`.
