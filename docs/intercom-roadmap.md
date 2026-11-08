# Intercom control panel: roadmap

Status: **everything here is built and merged into `main`.** That includes
v0.5.5 and all of v0.6.0: the monitor page (item 10), delivery confirmation
(item 11), GPIO triggers (item 12) and backup and restore (item 13). It also
has follow-ups: speaker offline alerts, per-site webhooks and monitor, and a
webhook delivery log. The owner chose to ship it all as **0.5.5**, so there's
no separate 0.6.0 release. The plan was written as two releases, **v0.5.5**
(integrate and polish) and **v0.6.0** (observe and harden).
`intercom-status.md` has the current state and the next steps.
`intercom-plan.md` is an overview of how the panel is built.

## Where it stood (v0.5.0, before this roadmap)

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
| Text-to-speech, visual alerts | some models | — | ✓ | ✓ text pages, spoken and shown (item 8) |

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

**Built.**
- A header button cycles System → Light → Dark, kept per browser in `localStorage` (`mcast.theme`).
- A tiny classic script (`js/theme.js`, first in `<head>`) sets `data-theme` before the first paint, so there's no flash. The strict CSP allows no inline script, so it's a file.
- The CSS's dark tokens apply under `prefers-color-scheme: dark` unless Light was chosen, and always when Dark was. That includes the Panel's palette.
- `mcast serve`'s page has the same switch.

The plan:

- System / Light / Dark in the header or Settings, kept per browser in
  `localStorage` (a device preference, not panel config).
- A `data-theme` attribute on `<html>`, set before first paint so the page
  doesn't flash. The CSS keeps `prefers-color-scheme` for System.
- Do the same in `mcast-web` so the two pages match.

### 6. Persistent history

**Built.**
- **Storage:** every run that ends (missed schedules included) is appended as a JSON line to `history/YYYY-MM-DD.jsonl` (UTC day) beside `intercom.json`. Each run is written once, even though run ids start again after a restart.
- **Retention:** days older than `history_days` (default 90; 0 keeps everything) are deleted at the first write of a day.
- **API:** `GET /history` filters by dates (in the panel's time zone), announcement, outcome, who started it and name, newest first and paged. `GET /history.csv` gives a spreadsheet, with formula-like cells defused. A category-limited token sees only its own runs.
- **History tab:** reads from the log, with plays in progress at the top, filters, "Show more" and Download CSV. It refreshes when a run ends.
- **Settings → History:** the number of days to keep.

The plan:

- An append-only JSONL run log beside `intercom.json`, rotated (keep 90 days
  by default, set in Settings).
- Each entry: announcement, trigger (button, schedule, API token name,
  push to talk), start and end, and the outcome per target.
- The History tab loads from the log, filters by announcement, zone,
  outcome and date, and exports CSV.

### 7. Panel stats strip

**Built.**
- **`GET /stats`** counts from the history log: plays today and over the last 7 days (in the panel's time zone), with played, stopped, failed and missed counted separately, plus groups busy or failed in the last 24 hours. A category-limited token counts only its own.
- **The strip** sits at the top of the Panel. It shows those numbers (problems in red), the next scheduled play, the mcast server when it's unreachable, and a link to History. It refreshes when a play ends and every minute.
- **Settings → Panel → Stats** turns it off.

The plan:

- Plays today and this week, the next scheduled play, busy or failed
  targets in the last 24 h, and whether the mcast server is reachable.
- Computed from the history log; kept small so the buttons stay the focus.

### 8. Playlists (requested)

**Built.**
- **Core:** `mcast serve` has a `files` source that plays up to 100 files as one stream, with no gap or SAP churn between them. It uses the first file's format and converts the rest, with an optional `gap_ms`. Loudness is measured over the whole list.
- **Panel:** playlists (`{"type": "sound"}` items, `gap_ms`) in a Playlists tab, with drag-to-reorder and lengths. An announcement's audio can be a playlist.
- **Parts:** each part sends other audio (a sound, a playlist or chime only) to other zones in the same run. Targets say which part they play. The trigger API's zone override narrows across parts.
- **Text pages** (built after; the owner chose both speech and displays, chosen per page, and shown in step with the audio):
  - A playlist item `{"type": "text", "text", "speak", "show", "voice"}`, both on by default.
  - **Speech is in the core:** a `files` source's items can be `{"say": TEXT, "voice"}`. `mcast serve` speaks them before starting, with Piper when it has a voice (the owner's choice), else eSpeak NG, or `--tts-command`, and caches the WAVs. `GET /voices`.
  - **In step:** senders report the item playing (`stats.item`, with a `job_updated` event as each starts). The panel shows a run's pages as each spoken page starts; a page only shown waits for the next item heard.
  - **Displays** (the owner chose both): a zone's `displays` get pages as real-time text through a text channel the panel keeps per group (`mcast text recv` shows them), and `?display=1&zones=...` is a full-screen display page in the browser.
  - The Pi installer installs eSpeak NG, or Piper and a voice (`TEXT_TO_SPEECH`).

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

**Built: the Settings tab and the Panel** (see 9a).
- The layout is kept in `intercom.json` (`layout`: tab → section ids).
- `PUT` and `DELETE /layout/{tab}` set and reset it; admins only.
- A reusable `arrangeable(tab, sections)` in the page adds *Arrange this page…*: a dialog to drag sections (or use the arrows), with a way back to the usual order.
- Sections are moved, not rebuilt, so what's typed in them stays.
- The Panel uses it too, for categories and the buttons in each (`panel`, `panel-buttons`).
- **Open:** whether to roll it out to other tabs with several sections (Schedules).

The plan:

Admins drag the sections of a tab into the order they want. The layout is kept in `intercom.json` and every screen shows it (good for wall tablets), with a way to reset it. Start with one tab (Settings, which has the most sections), then roll out to the Panel's buttons and the other tabs if it works well.

### 9a. Organizing the Panel (requested)

**Built: categories and tags.**
- **Categories:** each announcement can be in one (`category`), and each category is a heading on the Panel, with "Other" last. Categories are managed below the announcements list, which is grouped the same way.
- **Tags:** an announcement can have any number (`tags`), shown as filter chips on the Panel. The chosen chip is remembered per screen.
- **Arranging:** *✎ Customize* (admins) turns the Panel itself into an editor: make, rename, color, size and delete groups, drag buttons between them and groups into order, undo, and save it all at once (`PUT /panel-layout`, refused if the Panel changed meanwhile). Dragging works with a mouse, a finger (press and hold, or the ⠿ handle) and the keyboard (Space, arrows, Space). It replaced the *Arrange the Panel…* dialog. Every screen shows the same.

### 9b. Panel customization

**Built.**
- **Settings → Panel** (stored as `panel`): button size (small, medium or large), tiles or a one-row-per-button list, and whether to show the details line, "Next:" and "Now playing". Also asking before playing emergency announcements.
- **Categories:** a color and a button size.
- **Announcements:**
  - A color, overriding the category's.
  - An icon (an emoji).
  - "Ask before playing".
- **Colors** come from a fixed palette with separate light and dark shades. High and emergency keep their amber or red edge whatever the color.
- **Per screen:** categories fold away, remembered on that screen.

**Per-screen views and kiosk mode, built:**
- **Category-limited tokens:** an operator token can be limited to categories, and the server enforces it on play, stop, runs, push to talk and the trigger API. An announcement in no category isn't in any.
- **Kiosk mode:** `?kiosk=1` shows only the Panel, with a full-screen button, and keeps the screen awake.
- **URL narrowing:** `categories=`, `tag=`, `size=` and `mode=` narrow or size one screen's view. That's a view only, not a lock.
- **Link builder:** Settings → Panel builds the link.

The plan:

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

**Built.**
- **`GET /monitor?days=N`** adds up the history: totals, plays a day by outcome, per announcement, per zone (each send to a zone's group: played, busy, failed, taken over, using the zones the group is in now), schedule timeliness (on time, late over 5 s, missed, the latest start), and what started plays (type, and the top token names).
- **Connection uptime:** the panel logs connection changes and its own start and stop to `history/connection.jsonl`. Uptime counts only time the panel was running, and lists outages.
- **Monitor tab** (admins and operators): 7, 30 or 90 days, headline tiles, a stacked plays-a-day chart in status colors with a legend, hover details and a table view, and tables for announcements, zones, schedules, who started plays, and outages.
- A category-limited token sees only its own categories.
- **Still to do:** delivery confirmation from probes (item 11), which would add whether speakers heard each play.

The plan:

- Plays per announcement and per zone over time.
- Outcome rates per zone (played, busy, failed).
- Schedule reliability: on time vs late vs missed.
- Uptime of the connection to the mcast server.

### 11. Delivery confirmation ("probes")

**Built.** `mcast serve`'s monitor lists the receivers it hears (`listeners`
in `GET /api/v1/monitor`), and send jobs report their `ssrc`. A zone lists
its `listeners`; every play to it is checked (while it plays and up to 12 s
after), and the run, History, the Monitor tab's Speakers table, a toast and
a `listener_silent` webhook say who heard it and who stayed silent. See
*Who heard it* in the intercom's README. The plan:

- Endpoints running `mcast receive` already send named RTCP receiver reports
  (loss, jitter, playout delay). `mcast serve`'s monitor collects them
  (`Monitor::listeners()`), but didn't expose them: `MonitorSnapshot`
  in `mcast-control/src/watch.rs`, behind `GET /api/v1/monitor`, carried only
  groups and streams. Adding a `listeners` list there was the first step.
- A zone can list the listeners it expects. After a run, the panel shows
  "3/4 listeners heard it" and flags silent endpoints. A webhook fires for
  an endpoint that stays silent.
- Decided: no `tag` on send requests. Send jobs report their stream's
  `ssrc`, and each listener names the SSRC it hears (`about`), which ties
  listeners to a run exactly.
- Decided: "probes" means these `mcast receive` endpoints, not separate
  listening devices.

### 12. Triggers from the room

**Built.** `inputs` in the config: a GPIO line (on a Pi, its BCM number)
plays an announcement when pressed, or while held, read through the Linux
character device with `gpiocdev` (the owner chose that crate) and debounced
by the kernel. Settings → Buttons and contacts shows each one's state and
has a Test button; `pi/install.sh` gives the `mcast` user the `gpio` group.
An input plays at its announcement's priority, so a per-input priority
wasn't needed. The plan:

- Pi GPIO / contact-closure inputs mapped to announcements (a panic button,
  a door contact), with debounce and per-input priority.

### 13. Backup and restore

**Built.** Settings → Backup and restore, `mcast intercom backup|restore`,
and `GET /backup` / `POST /restore`: the config alone, or a `.tar.gz` with
every sound (the owner chose that, adding the small `tar` crate). A restore
first says what it adds, changes and removes, which sounds it uploads and
which would be missing; it keeps the config before as `intercom.json.bak`.
Tokens, webhooks and history are never in a backup. The plan:

- Export and import `intercom.json`, optionally as an archive with the sounds.
  Never include `tokens.json`, `webhooks.json` or history.
- Import validates first and shows what would change.

## Later / out of scope

- Users and roles beyond scoped tokens.
- SIP paging.
- (Text to speech and text to displays were built as text pages in
  playlists: see item 8.)

## Decisions

1. Probes are `mcast receive` endpoints and their RTCP reports (item 11).
2. Webhooks go both ways in v0.5.5: the trigger API in, events out (item 2).
3. Sound tags live in the panel's `intercom.json`, not the core (item 4).
4. Tokens: fixed roles, an optional announcement list on trigger tokens,
   hashed in a separate `tokens.json`, a GET trigger for trigger tokens only
   (`intercom-tokens.md`).
