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

### 1. Delivery confirmation ("probes"), roadmap item 11, in two PRs

"Probes" means speakers running `mcast receive`; the owner decided this.

**Correction to the first handoff:**
- `mcast serve`'s monitor does track listeners: `mcast_monitor::Monitor::listeners()` returns `ListenerReport`s with name, cname, loss, jitter, last heard and BYE.
- It watches configured groups and SAP-announced streams, so the panel's sends are covered.
- But `MonitorSnapshot` (`crates/mcast-control/src/watch.rs`) has only `groups` and `streams`, so `GET /api/v1/monitor` never shows listeners. Only the `mcast monitor` CLI prints them.

**1a. Core (backwards compatible).** Built on `claude/vibrant-knuth-34bpdf`: `GET /api/v1/monitor` and the `monitor` event now carry `listeners` (`docs/api.md`), tested with a named receiver on a real stream. Audio send jobs also report their `ssrc`, which matches a listener's `about`, so the panel can tell which listeners heard its own page; no `tag` on send requests is needed.
- Add `listeners: Vec<ListenerRow>` to `MonitorSnapshot`. Each row has the group, ssrc, from, name or cname, the ssrc it reports `about`, loss, jitter, last heard ms and `left`.
- Test with a sender and an `mcast receive --name` on loopback.

**1b. Panel:**
- A zone gains `expected_listeners` (the names `mcast receive --name` sets). Edit them in the Zones tab, offering the names currently heard.
- After each run, record per zone which expected listeners reported on that stream: `heard: {expected, heard, silent}` in the history entry.
- "Now playing" and History show "3/4 heard it", naming the silent ones.
- A new `listener_silent` webhook event.
- The Monitor tab gets a listeners table (runs expected, heard and missed, and last heard).
- The Zones tab shows each expected listener's last heard time.

### 2. Backup and restore, roadmap item 13

- `GET /backup` (admin) downloads `intercom.json` with a version and date. With `?sounds=1` it's an archive with the media folder.
- Never include `tokens.json`, `webhooks.json` or history.
- `POST /restore?dry_run=1` validates with the same checks as `mcast intercom check`. It returns what would change (announcements, zones, schedules and playlists added, changed or removed, and missing sounds).
- Without `dry_run`, it applies atomically and keeps the old file as `intercom.json.bak`.
- `mcast intercom backup` and `mcast intercom restore [--dry-run]`.
- Settings → Backup: download, and an upload that shows the dry run before applying.
- The workspace has `flate2` but no zip or tar crate. The recommendation is `.tar.gz` (adding `tar`); the other choice is JSON only.

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

1. Probes: how much loss still counts as "heard it"? Should listeners be checked between runs, when nothing plays? `mcast receive` sends reports only while receiving, so that may need a keep-alive.
2. GPIO: a small ioctl wrapper of our own, or the `gpiocdev` crate?
3. Backup: `.tar.gz` with the sounds (a new `tar` dependency), or JSON only?
4. Text pages in playlists: displays, TTS, or both?
5. Roll "Arrange this page" out to more tabs?

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
