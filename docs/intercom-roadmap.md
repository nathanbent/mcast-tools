# Intercom control panel: roadmap

Status: planning. v0.5.0 is released; this covers the next two releases,
**v0.5.5** (integrate and polish) and **v0.6.0** (observe and harden).
`intercom-plan.md` covers the original four phases, all built.

## Where it stands (v0.5.0)

| Area | Today |
|---|---|
| Triggering from other systems | `POST /api/intercom/v1/announcements/{id}/play` works with the panel's bearer token, but that token is all-powerful, the id is opaque, and it isn't documented as an integration API |
| Adding sounds remotely | The Sounds tab uploads through the panel (`PUT /api/intercom/v1/media/{name}`, passed on to the core's `PUT /media` with the core token) |
| File manager | Folders, rename/move (announcements follow), hide, preview, delete (refused while an announcement uses the file), drag-and-drop upload onto the tab. No tags, search, multi-select, or drag-to-folder |
| Theme | Follows the OS (`prefers-color-scheme`) only; no switch |
| Stats / monitoring | None. Run history is in memory: the last 50 runs, lost on restart |

## Compared with basic paging controllers

From general knowledge of the products (not checked against current
datasheets), so the cells are approximate.

| Feature | Algo 8301 | Axis Audio Manager Edge | InformaCast Basic / Valcom-type | mcast intercom (v0.5.0) |
|---|---|---|---|---|
| Zones / multicast groups | ✓ | ✓ | ✓ | ✓ (zones, sites) |
| Bell schedules, holidays | ✓ | ✓ | partial | ✓ (weekly, one-off, days off, groups) |
| Upload and manage a sound library | ✓ | ✓ | ✓ | ✓ (no tags or search) |
| HTTP API to trigger | ✓ (with auth) | ✓ (VAPIX) | ✓ | partial (admin token only) |
| Outbound events (webhooks) | partial | ✓ | ✓ | — (SSE only) |
| Contact closure / GPIO trigger | ✓ | ✓ (device I/O) | ✓ | — |
| Live paging | ✓ (SIP) | ✓ (SIP) | ✓ | ✓ (browser push to talk) |
| Endpoint health monitoring | via a separate tool | ✓ | partial | — (the core monitor has the data) |
| Logs / play history | ✓ | ✓ | ✓ | in memory only |
| Backup and restore | ✓ | ✓ | ✓ | — (copy `intercom.json` by hand) |
| Users and roles | single admin | ✓ | ✓ | single token |
| Text-to-speech, visual alerts | some models | — | ✓ | — (`mcast text` exists) |

Gaps worth closing beyond the original wish list: **persistent history**
(stats and monitoring depend on it) and **scoped tokens** (so a webhook or an
upload script doesn't hold the admin key). Contact-closure triggers and
backup/restore are standard on these boxes and cheap on a Pi.

## v0.5.5: integrate and polish

### 1. Scoped API tokens (foundation)

**Built**: see `intercom-tokens.md`. Fixed roles (`admin`, `operator`,
`trigger`, `media`), hashed in `tokens.json`, with expiry, usage, CLI
management and who-fired-it in history. Summary of the original sketch:

- Named tokens, created and revoked in Settings, each with a scope:
  - `play`: trigger and stop, optionally limited to listed announcements
    (became `trigger`, plus an `operator` role for wall tablets).
  - `media`: upload, list and delete sounds.
  - `admin`: everything.
- Stored hashed in the panel's config; the secret is shown once at creation.
- `--panel-token` / `MCAST_INTERCOM_TOKEN` stays as an admin token.
- Runs record which token (by name) fired them.

### 2. Trigger API and webhooks

**Built**: see `intercom-triggers.md`. `GET`/`POST /trigger/{name}`, `/stop`
and `/stop-all`; zone overrides are limited to the announcement's own zones.
Signed webhooks for runs, missed schedules and the mcast server's
connection, kept in `webhooks.json`, managed in Settings → Other systems.
The examples went in `intercom-triggers.md` rather than `docs/api.md`, which
covers `mcast serve`. The original sketch:

- `POST /api/intercom/v1/trigger/{name-or-id}`, so callers can use the
  announcement's name. The optional body overrides `zones` and `repeat`.
- `POST /trigger/{name-or-id}/stop` and stop-all, under the same `play` scope.
- A short "Integrations" section in `docs/api.md` with curl, Home Assistant
  and Node-RED examples.
- Outbound webhooks: POST JSON to configured URLs on `run_started`,
  `run_ended`, `run_failed`, `backend_down` and `schedule_missed`. Optionally
  signed with an HMAC header, with a short timeout and no retry storm.
- Decided: a GET form with `?access_token=` is allowed for `trigger` tokens
  only, for systems that can't set headers.

### 3. Remote sounds

**Built.** `mcast serve` checks uploads with `?audio=1` before they replace
anything (`422` if it can't play them) and reports each file's codec, rate,
channels and length (`/media?audio=1`, `/media-info/{name}`, probed once
per change). The panel always asks for both. The Sounds tab shows each
file's length and has a **Record** button (48 kHz mono WAV from the
browser's microphone, listen back, then save). Uploading with a `media`
token is documented; replacing a file keeps its name so announcements play
the new one. A WAV upload is stored as sent: nothing is converted. The
original sketch:

- Document `PUT /api/intercom/v1/media/{name}` for `media`-scope tokens.
- Check on upload that the file decodes (format, duration), so a bad file
  fails at upload instead of at play time.
- "Replace" uploads keep the name, so a system can push a fresh recording
  daily and the announcement plays the new one.
- A **Record** button on the Sounds tab, reusing the mic capture from push to
  talk (`talk.js`, `mic-worklet.js`).

### 4. File manager

**Built.**
- Tags are kept in `intercom.json` (`media_tags`). They follow moves and renames and go with deletes. `PUT /media-tags` tags or untags many files at once, case-insensitively.
- The Sounds tab:
  - A tag bar.
  - A search box over names and tags in every folder (`#tag` for a tag exactly).
  - Multi-select with move, add or remove tags, hide or unhide, and delete. Delete skips files in use and non-empty folders, and lists them.
  - Dragging rows onto folders, the path, or "Up a folder" moves them. Dropping files from the computer still uploads.
  - Sorting by name, length or size.
- The Announcements editor's sound list shows tags and has the same filter.

The original sketch:

- **Tags**: kept in `intercom.json`, the panel's own data, so the core stays
  untouched. They follow renames and moves the way `used_by` does. Edited in
  the file's row or in bulk.
- **Search**: one box matching names and tags across all folders. The
  announcement editor's sound picker gets the same filter.
- **Multi-select and bulk actions**: move, tag and untag, hide and show,
  delete. Bulk delete skips files that are in use and lists them.
- Drag files onto a folder to move them; sort by name, date or size (and
  duration once it's known).

### 5. Theme switch

- System / Light / Dark in the header or Settings, kept per browser in
  `localStorage` (a device preference, not panel config).
- A `data-theme` attribute on `<html>`, set before first paint so the page
  doesn't flash. The CSS keeps `prefers-color-scheme` for System.
- Do the same in `mcast-web` so the two pages match.

### 6. Persistent history

- An append-only JSONL run log beside `intercom.json`, rotated (keep 90 days
  by default, set in Settings).
- Each entry: announcement, trigger (button, schedule, API token name,
  push to talk), start and end, and the outcome per target.
- The History tab loads from the log, filters by announcement, zone,
  outcome and date, and exports CSV.

### 7. Panel stats strip

- Plays today and this week, the next scheduled play, busy or failed
  targets in the last 24 h, and whether the mcast server is reachable.
- Computed from the history log; kept small so the buttons stay the focus.

### 8. Playlists (requested)

**Built.**
- **Core:** `mcast serve` has a `files` source that plays up to 100 files as one stream, with no gap or SAP churn between them. It uses the first file's format and converts the rest, with an optional `gap_ms`. Loudness is measured over the whole list.
- **Panel:** playlists (`{"type": "sound"}` items, `gap_ms`) in a Playlists tab, with drag-to-reorder and lengths. An announcement's audio can be a playlist.
- **Parts:** each part sends other audio (a sound, a playlist or chime only) to other zones in the same run. Targets say which part they play. The trigger API's zone override narrows across parts.
- **Next:** text pages as playlist items.

The plan before building:

Decided so far:
- Playlists are their own named things, in a Playlists tab, and announcements use one in place of a single sound. Schedules and the trigger API follow from that.
- A playlist plays its items **in sequence**: sounds now, text pages later (through `mcast text`), maybe pauses. It plays as one run, with the announcement's chime before and after.
- **Several at once**: one run can send different audio to different zones, e.g. a message in two languages to two buildings.

To design before building:
- **Sequencing:** either the mcast server plays a list of files in one sender (a core change: no gap and no SAP churn between items), or the panel plays them one after another. The core change is recommended.
- **Several at once:** how an announcement maps zones to playlists or sounds. One idea is "parts", each a set of zones and what plays there.
- **Text pages:** which displays get them, and how they line up with the audio.

### 9. Arrangeable layout (requested)

**Started: the Settings tab.**
- The layout is kept in `intercom.json` (`layout`: tab → section ids).
- `PUT` and `DELETE /layout/{tab}` set and reset it; admins only.
- A reusable `arrangeable(tab, sections)` in the page adds *Arrange this page…*: a dialog to drag sections (or use the arrows), with a way back to the usual order.
- Sections are moved, not rebuilt, so what's typed in them stays.
- **Next, if it works well:** the Panel's buttons, then other tabs with several sections (Schedules).

The plan:

Admins drag the sections of a tab into the order they want. The layout is kept in `intercom.json` and every screen shows it (good for wall tablets), with a way to reset it. Start with one tab (Settings, which has the most sections), then roll out to the Panel's buttons and the other tabs if it works well.

### 9a. Organizing the Panel (requested)

**Built: categories and tags.**
- **Categories:** each announcement can be in one (`category`), and each category is a heading on the Panel, with "Other" last. Categories are managed below the announcements list, which is grouped the same way.
- **Tags:** an announcement can have any number (`tags`), shown as filter chips on the Panel. The chosen chip is remembered per screen.
- **Arranging:** *Arrange the Panel…* (admins) orders the categories and the buttons inside each, through the layout (`panel`, `panel-buttons`). Every screen shows the same.

### 9b. Panel customization (planned)

A plan to review before building. It's the same for every screen unless "per screen" says otherwise.

- **Button size:** small, medium or large, for the whole Panel with an override per category. Medium is today's. Small fits about twice as many on a tablet; large is for touch kiosks.
- **Button color:** per category, with an override per announcement. Colors come from a fixed palette (about 8 named colors such as red, orange, amber, green, teal, blue, purple, grey), so text stays readable in light and dark themes. A free color picker would make unreadable combinations easy. The top border keeps showing priority unless a color is set; emergency stays red regardless.
- **Icon or emoji** on a button (a bell, a siren, a door), chosen from a short built-in set or any emoji.
- **Compact list mode:** one row per announcement instead of tiles, for screens with many announcements.
- **What a button shows:** turn the zones line, repeat count and priority badge on or off.
- **Panel extras:** show or hide "Next:" and "Now playing", and collapse categories (remembered per screen).
- **Confirm before playing:** per announcement, or for every emergency one ("Play Lockdown to all zones?").
- **Per-screen views** (later, with tokens): an operator token limited to some categories, so a reception tablet shows only its buttons; a kiosk URL that hides the tabs and header.

Storage would be a `panel` object in `intercom.json` (size, mode, what to show), plus `color` and `icon` on categories and announcements. The arrange dialog would become the place to set them.

## v0.6.0: observe and harden

### 10. Monitor page

- Plays per announcement and per zone over time.
- Outcome rates per zone (played, busy, failed).
- Schedule reliability: on time vs late vs missed.
- Uptime of the connection to the mcast server.

### 11. Delivery confirmation ("probes")

- Endpoints running `mcast receive` already send named RTCP receiver reports
  (loss, jitter, playout delay), which the core monitor collects.
- A zone can list the listeners it expects. After a run, the panel shows
  "3/4 listeners heard it" and flags silent endpoints. A webhook fires for
  an endpoint that stays silent.
- This needs the optional core change from `intercom-plan.md`: a free-form
  `tag` on send requests, echoed in `JobInfo`, to match jobs reliably.
- Decided: "probes" means these `mcast receive` endpoints, not separate
  listening devices.

### 12. Triggers from the room

- Pi GPIO / contact-closure inputs mapped to announcements (a panic button,
  a door contact), with debounce and per-input priority.

### 13. Backup and restore

- Export and import `intercom.json`, optionally as a zip with the sounds.
- Import validates first and shows what would change.

## Later / out of scope

- Users and roles beyond scoped tokens.
- Text-to-speech announcements.
- SIP paging.
- Text to displays alongside an announcement, through `mcast text`. That
  would be unusual among these products.

## Decisions

1. Probes are `mcast receive` endpoints and their RTCP reports (item 11).
2. Webhooks go both ways in v0.5.5: the trigger API in, events out (item 2).
3. Sound tags live in the panel's `intercom.json`, not the core (item 4).
4. Tokens: fixed roles, an optional announcement list on trigger tokens,
   hashed in a separate `tokens.json`, a GET trigger for trigger tokens only
   (`intercom-tokens.md`).
