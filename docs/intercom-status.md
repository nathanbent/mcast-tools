# Intercom: where things stand (handoff)

Last updated 2026-11-08, in a second session that checked the first one's work
and planned what's left. Read this first, then `intercom-roadmap.md`, which has
the full plan and what each item became.

## The branch and PR

- Branch `claude/intelligent-hypatia-awklrl`, open as **nathanbent/mcast-tools#38**, not merged yet. It's waiting on the owner's review; there are no review comments.
- `main` (v0.5.4) is merged in. `main` keeps moving (v0.5.1 → v0.5.4 came from other threads), so merge `origin/main` into the branch before pushing more. Conflicts so far were small, in `announcements.js` and `talk.js`.
- The PR description lists everything in detail.
- The docs fixes and this plan were made on `claude/vibrant-knuth-34bpdf`, branched from #38's head (`6908d0f`), to go into #38 before it merges.

## Checked (2026-11-08)

On #38's head (`6908d0f`), locally:
- `cargo fmt --all --check` passes.
- `cargo clippy --workspace --all-targets --locked -- -D warnings` passes.
- `cargo test --workspace --locked` passes: 271 tests, none failing or ignored.

GitHub CI was green on the same commit. Every item listed below is in the code, with no TODOs left. The Home Assistant and Node-RED examples promised for item 2 are in `intercom-triggers.md`.

Docs that had fallen behind the code, now fixed:
- The roadmap's status line still said "planning".
- Item 9 said only Settings was arrangeable; the Panel is too.
- Item 11 said the core monitor already exposes listener reports. It doesn't (see below).
- `intercom-tokens.md` described persisted history as future work.

## What's built (all in #38)

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

## What's next (the plan)

In this order. Each numbered step is its own PR unless it says otherwise.

### 0. Merge #38 and release v0.5.5

The docs fixes above go in first.

### 1. Delivery confirmation ("probes"), roadmap item 11: built

On `claude/vibrant-knuth-34bpdf`, on top of #38. "Probes" means speakers running `mcast receive --name`; the owner decided this.

**1a. Core (backwards compatible):**
- `GET /api/v1/monitor` and the `monitor` event carry `listeners`: each receiver heard on a watched group's RTCP port, with the SSRC it hears (`about`).
- Audio send jobs report their `ssrc`, so a listener's `about` ties it to a page exactly. No `tag` on send requests was needed.
- Documented in `docs/api.md`. Tested in `mcast-control/src/watch.rs` with a real sender and a named receiver.

**1b. Panel:**
- **Zones:** a zone's `listeners` (names). The Zones tab offers the names the mcast server hears now (`GET /listeners`).
- **Checking runs:** the runner checks every run to zones that list listeners. It asks `/monitor` every second while the run plays and for up to 12 s after it ends, stopping early once everyone expected was heard.
- **Results:** the run gets `listeners: {checking, expected, heard [{name, loss_percent}], silent, error}`, and each target its `expected` and `heard`.
- **Where it shows:**
  - History waits for the check before writing the run.
  - A `listener_silent` webhook, and a toast on the page.
  - Run cards say "Heard by 1 of 2 · silent: Office".
  - The Monitor tab has a Speakers table and a "Heard by every speaker" tile.
- **The monitor off (or unreachable)** sets `listeners.error`, and nobody is called silent.
- **Tests:**
  - Runner, history, webhook and summary unit tests, plus an API test, against the fake.
  - An end-to-end test in `tests/server.rs`: a real `mcast serve` with its monitor on and a real named receiver.
  - Checked by hand in a browser with the real binaries: the zone form, the toast, History and Monitor, in light and dark, at desktop and phone widths.
- **Not done (small):** showing each expected listener's last-heard time in the Zones list.

### 2. Backup and restore, roadmap item 13: built

On the local branch `backup-restore`, on top of #43; not pushed yet (see the note for the owner below).
- **The owner chose** config plus optional sounds. It adds the `tar` crate (with `filetime`; `flate2` was already in the tree).
- **`crate::backup`** holds the shared logic:
  - writing and reading the `.tar.gz` (`mcast-intercom-backup.json`, `intercom.json`, `media/PATH`), with safe paths, at most 1 GiB and 10,000 files;
  - reading a plain `intercom.json` as a backup too;
  - `media_used`, `plan` (lists added, changed and removed by id; other settings; sounds to upload; missing sounds) and `apply` (upload, keep `.bak`, save).
- **API:** `GET /backup[?sounds=1]` and `POST /restore[?dry_run=1]`, admin only, under the config lock.
- **CLI:** `mcast intercom backup [--sounds] [-o FILE]` and `mcast intercom restore FILE [--dry-run]`.
- **Page:** Settings → Backup and restore. Download either form, or choose a file to see what it would change, then Restore.
- **Tests:** unit tests (round trip, refusing bad archives and `../` paths, planning) and an API test (download both forms, dry run, restore with a sound, `.bak`, a bad file, a sound that isn't audio).
- **Checked by hand:** the CLI against a real `mcast serve` with a real WAV, and the page in a browser (download, preview, restore, a junk file, phone width and dark).

### 3. Triggers from the room (GPIO), roadmap item 12

- An optional `inputs` list in `intercom.json`, read by `mcast intercom serve`. Each input has:
  - a pin (BCM number), `active_low`, `debounce_ms` (default 50) and a priority;
  - an announcement, and `on`: press, release, or both (play on close, stop on open).
- Read through the Linux GPIO character device (`/dev/gpiochip0`, v2 uAPI), not the deprecated sysfs. Use either a small ioctl wrapper or the `gpiocdev` crate, if its MSRV fits 1.75.
- Linux only. Elsewhere `check` refuses `inputs` with a clear message.
- Settings → Inputs shows each pin's live state and has a test button.
- Runs record `trigger: {type: "input", pin, name}`.
- Pi image: add the `mcast` user to the `gpio` group, and document the wiring (pull-ups, a contact closure to GND).
- Test debounce and the edge-to-action logic against a fake pin source; check the real device by hand on a Pi.

### 4. Text pages in playlists

- `PlaylistItem` (`model.rs`) has one variant, `Sound`, and is ready for another.
- Blocked on the owner: show text on displays through `mcast text`, read it aloud (TTS, listed as out of scope), or both.

### 5. Smaller follow-ups

- Per-site webhook filters.
- A webhook delivery log (the last N deliveries per hook).
- Monitor charts per site.
- "Arrange this page" on more tabs (Schedules), if the owner wants it.

Then release v0.6.0.

## Questions still open for the owner

1. Probes: how much loss still counts as "heard it"? For now any report naming the stream counts, and the worst loss is recorded so a threshold can be added. Should listeners be checked between runs too? Receivers report every few seconds even when idle (`about` is then `null`), so a "speaker offline" alert is possible.
2. GPIO: a small ioctl wrapper of our own, or the `gpiocdev` crate?
3. Text pages in playlists: displays, TTS, or both?
4. Roll "Arrange this page" out to more tabs?

## How to work on it

- **Build:** `cargo build -p mcast`. Needs `libasound2-dev`, `libopus-dev` and `pkg-config` (apt).
- **Checks** (CI runs the same): `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- **MSRV is 1.75:** avoid newer std APIs (for example, use `map_or(true, …)`, not `is_none_or`).
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
    - `store.js`: `onRunEnded`, `panelAnnouncements`, `can`, `TABS`.
    - `editor.js`: `mountEditor`, `mediaFilter`, `colorSelect`.
- **Gotcha:** `pkill -f 'mcast serve'` inside a script can kill the script itself. Use a pattern like `'[t]arget/debug/mcast serve'`.
