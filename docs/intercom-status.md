# Intercom: where things stand (handoff)

Written 2026-11-08 at the end of a long working session, for the next one.
Read this first, then `intercom-roadmap.md`, which has the full plan and what
each item became.

## The branch and PR

- Branch `claude/intelligent-hypatia-awklrl`, open as **nathanbent/mcast-tools#38**, not merged yet.
- CI was green on the last push (`b26a2da`), and `main` (v0.5.4) was merged in.
- There were no review comments. It's waiting on the owner's review.
- `main` keeps moving (v0.5.1 → v0.5.4 came from other threads). Merge `origin/main` into the branch before pushing more. Conflicts so far were small, in `announcements.js` and `talk.js`.
- The PR description lists everything in detail.

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

## What's next

From `intercom-roadmap.md` (v0.6), in a sensible order:

1. **Delivery confirmation ("probes"), item 11.** "Probes" means speakers running `mcast receive`; the owner decided this.
   - Their named RTCP receiver reports are already collected by `mcast serve`'s monitor (`GET /api/v1/monitor`).
   - A zone lists the listeners it expects. After a run, the panel shows "3/4 heard it", flags silent ones, and fires a webhook.
   - It probably wants the optional core change: a free-form `tag` on send requests, echoed in `JobInfo`, to match jobs to runs.
   - It would also feed the Monitor tab.
2. **Triggers from the room (GPIO), item 12.** Pi GPIO or contact-closure inputs mapped to announcements, with debounce and a priority per input.
3. **Backup and restore, item 13.**
   - Export and import `intercom.json`, optionally as a zip with the sounds.
   - Never include `tokens.json` or `webhooks.json`, which hold secrets.
   - Validate before importing and show what would change.
4. **Text pages in playlists.** The `PlaylistItem` enum is ready for a new kind of item. Still waiting on the owner to say what a text page does: show on displays through `mcast text`, be read aloud (TTS), or something else.
5. **Smaller ideas noted but not done:**
   - Per-site webhook filters.
   - A webhook delivery log.
   - Editing a zone's expected listeners (with item 1).
   - Showing Monitor charts per site.

## Questions still open for the owner

- What text pages in playlists should do.
- Whether to roll "Arrange this page" out to more tabs (Schedules and others). It's on Settings and the Panel now.

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
