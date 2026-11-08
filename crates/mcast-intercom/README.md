# mcast-intercom

An intercom control panel for `mcast serve`. You set up **zones** (sets of speakers)
and **announcements** (an audio file or a chime, which zones it goes to, how
many times, at what priority), then play them from a button or on a
**schedule**.

It is a client of the mcast HTTP API and nothing else: it doesn't use any
other crate in this workspace, so it can be moved out on its own. See
[`docs/intercom-plan.md`](../../docs/intercom-plan.md) for an overview of how it's built, and [`docs/intercom-roadmap.md`](../../docs/intercom-roadmap.md) for what's next.

## Use

`mcast serve` must be running. The intercom talks to it at `--url`
(`$MCAST_URL`, default `http://127.0.0.1:8650`) with `--token` (`$MCAST_TOKEN`).

```bash
mcast intercom list            # zones and announcements
mcast intercom check           # the config, and its media files and phonebook names on the server
mcast intercom upcoming        # what the schedules play next
mcast intercom play lunch      # by id or name; Ctrl-C stops it
mcast intercom backup --sounds # intercom-DATE.tar.gz: the config and every sound
mcast intercom restore intercom-2026-11-08.tar.gz --dry-run
```

### The panel

```bash
mcast intercom serve                                          # open http://127.0.0.1:8651/
MCAST_INTERCOM_TOKEN=change-me mcast intercom serve --listen 0.0.0.0:8651   # https://HOST:8651/
```

Listening on the network, the panel serves **HTTPS** (with a self-signed
certificate unless you give it one; see Push to talk below). On loopback it's
plain HTTP.

A page with a tab for each part:

| Tab | |
|---|---|
| **Panel** | A **stats strip** (plays today and over the last 7 days, with failed and missed ones; groups busy or failed in the last 24 hours; and the mcast server if it's unreachable), then a big button for each announcement, under its **category**'s heading, what's playing now (with Stop) and what's next. **Tag** chips show just one tag's buttons (remembered per screen). Admins **customize** it right on the Panel (*✎ Customize*): make, rename, color, size and delete groups (categories), and drag buttons between groups and groups into order, with a mouse, a finger (press and hold) or the keyboard, then save it for every screen in one go, and **how it looks** in Settings → Panel (button size, tiles or a list, what's shown, asking before emergencies); categories and announcements can have a **color** (and announcements an **icon**, and "ask before playing"). Each screen can fold categories away. A live announcement's button is **push to talk** (below). *All stop* in the header stops everything |
| **Monitor** | From the history, over the last 7, 30 or 90 days: totals (plays, how many played, failed and missed, schedules on time, the mcast server's uptime), plays a day by outcome (a chart, or a table), each announcement and zone with its outcomes (busy, failed, taken over), how schedules kept time (late is over 5 s), what started plays, which **speakers** heard plays and which missed them, and when the mcast server was unreachable. With sites, it can show **one site**: its plays, zones and speakers (the mcast server's uptime stays the panel's) |
| **History** | Every play, and how each group fared, kept on disk (a file a day in `history/` beside `intercom.json`, for 90 days or as set in Settings → History): filter by date, announcement, outcome and who started it, page back, and **download a CSV** |
| **Schedules** | When announcements play by themselves, under their **groups** (each with an on/off switch); what's coming up; *Pause all*; and **days off** (holidays, breaks) when no schedule plays. With **sites**, a bar picks all sites (a card for each: its time zone, what's next, pause) or one site (its schedules, pause switch and own days off) |
| **Announcements** | Listed under their categories, with the **categories** themselves below. Each one in sections: General (name, audio: a sound, a playlist, chime only or live), On the Panel (category, tags), Chimes, Zones, **Parts** (other audio to other zones in the same play), Playback (repeats, priority) and Sound (processing, volume, lead-in) |
| **Playlists** | Sounds and **text pages** that play one after another as one announcement: add them, drag (or use the arrows) to reorder, listen, and set the gap between them. Each text page is spoken, shown on displays, or both, with a voice from the mcast server's (see *Text pages* below) |
| **Sounds** | The mcast server's sound files, in folders, with each one's length: upload (or drop files on it), **record** one from the browser's microphone, listen, **tag**, **search** names and tags in every folder (`#tag` for a tag), sort, rename or move (or **drag** onto a folder), hide from the lists, delete. **Select several** to move, tag, hide or delete them together. Uploads must be audio the mcast server can play. Renaming or moving a file updates the announcements that use it, and keeps its tags; one in use can't be deleted. The Announcements editor can filter its sound list the same way |
| **Zones** | Each zone's multicast groups, with the codec, volume, TOS, TTL and interface its speakers need, the **listeners** expected to hear it (see *Who heard it* below), with the receivers the mcast server hears now offered to add, and its **text displays**. The list shows each listener's state: reporting to the mcast server now or not, and when it last heard a page |
| **Settings** | Announcement defaults (chimes, sound, and the codec for zone groups), the time zone, **sites**, which chimes the lists show, **access tokens**, **other systems** (trigger URLs and webhooks), **buttons and contacts** (GPIO inputs), **backup and restore**, and the connection to the mcast server. Admins can **arrange** its sections (*Arrange this page…*), and every screen shows that order |

The setup tabs (Playlists, Sounds, Zones, Settings) are under **More** in
the header. The header's theme button picks the theme: the system's, light or dark,
remembered in that browser (`mcast serve`'s own page has the same switch).

Sound files and phonebook names come from the mcast server, so its own web
page and TUI see the same files and folders.

Schedules only play while `mcast intercom serve` runs.

The panel is its own server with its own keys: a startup token
(`--panel-token` or `$MCAST_INTERCOM_TOKEN`), which is an admin key, and
named [access tokens](#access-tokens) with roles. Listening beyond loopback
needs the startup token or an admin access token. `--token` stays the mcast
server's. It saves edits to `intercom.json`, reading the file
again first, so changes made by hand while it runs aren't lost. It keeps the
last 50 plays in memory for the Panel, and every play that ends in the
history on disk. When it stops, anything playing finishes on the mcast
server, but repeats still to come are dropped.

On a Raspberry Pi, `ENABLE_INTERCOM=yes` in `pi/pi.conf` sets it up as a
service next to `mcast serve` ([pi/README.md](../../pi/README.md#the-intercom-panel)).

### Screens and kiosks

Open the panel with `?kiosk=1` on a wall tablet: only the Panel shows (no
tabs), with a full-screen button, and the screen is kept awake. The URL can
also narrow and size what that screen shows: `categories=bells,other`
(`other` for announcements in none), `tag=Building A`, `size=large`,
`mode=list`. Settings → Panel builds the link. Narrowing by URL is only a
view: to make sure a screen can only play some announcements, sign it in with
an operator token limited to those categories.

```
https://pager.local:8651/?kiosk=1&categories=reception&size=large#panel
```

**Display page.** `?display=1&zones=hall,gym` makes the page a sign for a TV
or tablet: full screen, kept awake, showing the text pages of what plays to
those zones (all zones without `zones=`) in large type as the playlist
reaches them, and the time in between. Sign it in with an operator token;
one limited to categories shows only theirs. Settings → Panel builds the
link.

### Text pages

A playlist item can be a text page, spoken, shown, or both (the default):

- **Spoken** by the mcast server's text to speech, as the playlist reaches
  it, in the announcement's stream (chimes, processing and volume apply).
  `mcast serve` uses Piper if it's installed with a voice, else eSpeak NG
  (see `docs/api.md`, *Text to speech*); `GET /voices` lists what it has.
  It keeps what it spoke, so a page is only synthesized the first time.
- **Shown** as the playlist reaches it: when the page is spoken, as its
  speech starts; a page that's only shown appears with the next item heard
  (or with the last, at the end). It goes to:
  - each zone's **text displays**: groups that get it as real-time text
    (RFC 4103), shown by `mcast text recv -g GROUP` on any screen. The
    panel keeps a text channel open per group (named "Intercom") while it's
    used, and closes it after five idle minutes;
  - the **display page** (above), and the run card.

  A page stays up while the run plays, and for 30 s after it ends; a
  stopped run's come down at once.
- **Parts** show their own playlist's pages, on their own zones' displays.

It needs an mcast server from 0.5.5 on: older ones refuse spoken items,
and don't say when each item starts.

### Access tokens

Give each person, screen or system its own token, with only what it needs.
Each token has one role:

| Role | For | Can |
|---|---|---|
| `admin` | Whoever sets the panel up | Everything, tokens included |
| `operator` | A wall tablet, a reception desk | The Panel, History and Sounds tabs: play, stop, push to talk, listen. Changes nothing. Optionally limited to some **categories**: it then sees, plays and stops only announcements in them |
| `trigger` | Other systems (a doorbell, a PBX, Home Assistant) | Play announcements through the API, and stop them. Optionally limited to some announcements |
| `media` | A recording or upload script | Sound files: list, upload, download, delete, folders, move, hide |

Make them in **Settings → Access tokens**, or from the command line:

```bash
mcast intercom token add "Front doorbell" --role trigger --announcement doorbell
mcast intercom token add Reception --role operator --category reception --expires 2027-06-30
mcast intercom token list
mcast intercom token revoke "Front doorbell"
```

A token's secret (`mci_...`) is shown once, when it's made: only its SHA-256
is kept, in `tokens.json` beside `intercom.json` (or `--tokens FILE` /
`$MCAST_INTERCOM_TOKENS`), readable by its owner only. If a secret is lost,
revoke the token and make another. A token can expire at the end of a given
day, in the panel's time zone. The panel notices changes to the file while it
runs, so a token revoked from the command line stops working at once. Each
play records which token started it, and the History tab shows it.


On loopback with no startup token the panel is open, as before: requests
without a token act as admin. A known access token still gets its own role.

### From other systems

**Sounds.** A `media` token can upload sounds from a script, such as a
message recorded fresh each day under the same name, which every
announcement using that name then plays:

```bash
curl -X PUT -H "Authorization: Bearer mci_..." --data-binary @today.wav https://HOST:8651/api/intercom/v1/media/daily/message.wav
```

The mcast server checks it's audio it can play first; if it isn't, the
upload is refused (`422`) and the old file stays.

**Trigger API.** Other systems play announcements by id or name with a
trigger token (or an operator or admin one):

```bash
curl -X POST -H "Authorization: Bearer mci_..." https://HOST:8651/api/intercom/v1/trigger/doorbell
curl -X POST -H "Authorization: Bearer mci_..." https://HOST:8651/api/intercom/v1/trigger/doorbell/stop
```

A device that can only fetch a URL can use a GET with the token in it
(trigger tokens only):
`https://HOST:8651/api/intercom/v1/trigger/doorbell?access_token=mci_...`.
`zones=reception` plays to only some of the announcement's zones, and
`repeat=3` changes how often it plays, for that play only. These go in the
URL, or in a JSON body (`{"zones": ["reception"], "repeat": 3}`).
`/stop-all` stops everything the token may stop. Settings → Other systems
shows the URLs for each announcement.

**Webhooks.** The panel POSTs JSON to other systems when an announcement
starts, ends or fails, when a schedule is missed, and when the mcast server
goes away or comes back. Add them in Settings → Other systems, choose the
events each one gets, and use *Test* to check one. Each delivery is signed
(`X-Mcast-Signature`, an HMAC-SHA256 with the hook's secret, shown once)
unless you turn that off. *Log* lists a hook's last 50 deliveries, with each
one's result. Hooks are kept in `webhooks.json` beside `intercom.json`,
readable by its owner only, and their deliveries in `webhook-log.jsonl`.

The payloads, signature check, and Home Assistant and Node-RED examples are
in [docs/intercom-triggers.md](../../docs/intercom-triggers.md).

### Push to talk

An announcement whose audio is **Live** ("Talk to everyone", "Talk
upstairs") pages with the browser's microphone. **Hold** its button to talk
and let go to end, or **tap** it to talk hands-free and tap again to end;
with the keyboard, Enter or Space starts and ends. While you talk, the
button shows *On air* and the microphone's level.

It plays like any other announcement: to its zones, with its priority,
chimes before and after (the defaults' unless it sets its own), processing
and volume; it shows under *Now playing*, and Stop and *All stop* end it.
You can talk straight away: what you say during the opening chime plays
after it, so listeners hear you a chime's length late. When you let go, the
rest plays out, then the end chime.

A page only starts once the microphone is giving audio: if it gives none
within 3 seconds, nothing is paged and the panel says so (its console has
the details). During a page, the panel warns when the audio stops coming.
If the page comes out quiet, raise the announcement's **Input gain** (Sound,
in its editor), turn on **Normalize loudness**, or choose **Voice**
processing, which normalises too and also cleans up speech. Normalising
levels the page out within about a second of talking. A very quiet
microphone may need some input gain as well, since normalising boosts by
at most 24 dB.

If no audio comes from the browser for 20 seconds, the page ends by itself
(so a page left talking doesn't hold its zones for ever), and the panel
says why. Locking a phone's screen, or leaving the panel in a background
tab, can pause the microphone and end a hands-free page this way.

Browsers only allow the microphone on **HTTPS** pages (and on
`http://localhost`), so the panel serves HTTPS whenever it listens on
anything but loopback:

- By default it makes a **self-signed** certificate the first time and keeps
  it beside the config (`intercom-cert.pem`, `intercom-key.pem`). It names
  this machine (host name, `NAME.local`, its address) and `localhost`; add
  more with `--tls-name` (e.g. its DNS name) before it's first made, or
  delete the two files to make a new one. Browsers warn about it once;
  accept it to go on.
- `--tls-cert FILE --tls-key FILE`: your own certificate (PEM), e.g. from
  your organisation's CA, so there's no warning.
- `--no-tls`: plain HTTP anyway (behind a proxy that adds HTTPS, say). Live
  buttons then say they need HTTPS instead of talking, except on localhost.
- On loopback it's plain HTTP unless you ask (`--tls-self-signed`).

These can also come from `$MCAST_INTERCOM_TLS_CERT`,
`$MCAST_INTERCOM_TLS_KEY`, `$MCAST_INTERCOM_TLS_SELF_SIGNED` and
`$MCAST_INTERCOM_NO_TLS` (`true`).

The first time you talk, the browser asks to use the microphone. Firefox
asks every time unless you tick *Remember this decision*. If you let go of
the button to answer, the page carries on hands-free: tap to end it.

The audio goes from the browser to the panel over a WebSocket, as 48 kHz
mono, and on to `mcast serve` as one live sender per group. Live
announcements can't be scheduled, and play once.

`--web-dir crates/mcast-intercom/assets` serves the page from disk, so a
reload picks up edits; `--no-web` serves the API alone.

### Buttons and contacts (GPIO)

On a Raspberry Pi (or any Linux machine with GPIO), the panel can watch pins
and play an announcement when one is pressed: a panic button, a door contact,
a bell push. Set them up in Settings → Buttons and contacts.
- **What it shows:** each one's state now (pressed, released, or why it
  can't be read), and a **Test** button.
- **A press** plays the announcement, at its own priority. Pressing again
  while it plays does nothing.
- **"Play while held"** stops it when the button is let go: good for a
  door held open.
- **At start-up:** a contact that's already closed when the panel starts
  doesn't page.
- **In the history:** runs say which input started them (`trigger: {"type":
  "input", "id", "by": NAME}`).

The pins are read through the Linux GPIO character device (`/dev/gpiochipN`),
and the kernel debounces them. The user running the panel needs access to
it: on Raspberry Pi OS, the `gpio` group (`pi/install.sh` sets that up). On
other systems the inputs show as unreadable.

```json
"inputs": [
  {"id": "panic", "name": "Panic button", "line": 17, "announcement": "lockdown"},
  {"id": "door", "name": "Loading dock door", "line": 27, "announcement": "door-open",
   "while_held": true, "debounce_ms": 200}
]
```

Each input has these fields:
- `line`: the GPIO number (BCM numbering on a Pi; GPIO17 is header pin 11).
- `chip`: `gpiochip0` unless set.
- `active_low`: default `true`, meaning pressed when the pin reads low, as
  with a switch to ground.
- `bias`: `pull_up` (the default), `pull_down` or `none`.
- `debounce_ms`: default 50, at most 1000.
- `announcement`: any but a live one.
- `while_held`: stop the announcement on release.
- `enabled`: default `true`.

Two enabled inputs can't share a line, and an announcement an input plays
can't be deleted.

### Backup and restore

A backup is the config (`intercom.json`) on its own, or a `.tar.gz` with
every sound on the mcast server too. Access tokens, webhooks and the play
history are never in it: tokens and webhooks hold secrets.
- **Making one:** Settings → Backup and restore, `mcast intercom backup
  [--sounds] [-o FILE]`, or `GET /backup[?sounds=1]`.
- **What a restore says first:**
  - which zones, announcements, schedules, playlists, categories, sites and
    schedule groups it adds, changes or removes;
  - which other settings change;
  - which sounds it would upload;
  - which sounds the restored config uses that are neither on the mcast
    server nor in the backup.
- **Restoring:** it uploads the backup's sounds the mcast server doesn't
  have, or has at another size, and the server checks they're audio. Then
  it replaces the whole config, keeping the one before as
  `intercom.json.bak`. If a sound can't be uploaded, the config isn't
  changed.
- **Where:** in Settings, or with `mcast intercom restore FILE
  [--dry-run]`. A running panel picks up a restore made from the command
  line.
- **Size:** a backup holds up to 1 GiB.

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
`listeners`: the names of the speakers expected to hear its pages (see
*Who heard it*), e.g. `["Lobby", "Office"]`; up to 100, each once.
`displays`: text displays, groups (phonebook names or addresses) that get
the text pages of what plays here (see *Text pages*); up to 20.

**Announcements.** `id`, `name` and `zones` (zone ids), then:

| Field | Default | |
|---|---|---|
| `audio` | | `{"media": "NAME"}`, a file uploaded to the server, `{"playlist": "ID"}`, `"chime_only"`, or `"live"` (push to talk from the panel's page) |
| `category` | none | The Panel heading it's under (a category's id); none puts it under "Other" |
| `tags` | none | For filtering the Panel: up to 20, each up to 32 characters, no commas |
| `color` | its category's | Its button's color: `red`, `orange`, `amber`, `green`, `teal`, `blue`, `purple` or `grey` (high and emergency keep their amber or red edge) |
| `icon` | none | An emoji (up to 4 characters, no spaces) on its button |
| `confirm` | `false` | Ask "are you sure?" before playing it from the Panel |
| `parts` | none | Other audio to other zones in the same play: `[{"zones": [...], "audio": ...}]`, `audio` as above but not live. A zone can be in only one place: the announcement's own `zones` or one part |
| `chime`, `end_chime` | the defaults' | As in the API: `"default"` (the mcast server's: `rising` before, `falling` after), a built-in (`"ding-dong"`, `"westminster"`, `"church-bell"`, `"siren-wail"`, ...; `mcast audio chimes` lists them), or `{"file": "NAME"}`; or `"none"` for no chime even when the defaults have one. A `"chime_only"` announcement left with no chime at all plays the mcast server's default chime |
| `mode` | the defaults' | `voice`, `music` or `none` |
| `volume_db`, `lead_in_ms` | the defaults' | |
| `input_gain_db` | the defaults' | Gain on the audio before processing (not the chimes), -40 to 40 dB: for a live announcement, the microphone's input gain |
| `normalize` | the defaults' | `true` levels out quiet and loud audio (to -16 LUFS) even without processing; `voice` and `music` always do |
| `repeat` | `1` | Times to play it (up to 20); each repeat has its chimes |
| `repeat_gap_s` | `2` | Seconds between repeats |
| `priority` | `normal` | What happens to a group something else is already sending to (below) |

**Categories.** `[{"id", "name", "color", "size"}]`: headings on the Panel,
with an optional color and button size (`small`, `medium`, `large`) for their
buttons. Their order, and
the buttons' order, are in `layout` (`panel` and `panel-buttons`), set with
*✎ Customize* on the Panel. Deleted there, a category's buttons move to Other;
through `DELETE /categories/{id}`, one with announcements in it can't be.

**`history_days`**: days of play history to keep (default 90; 0 keeps it
all).

**`panel`**: how the Panel looks: `{"size": "medium", "mode": "tiles",
"show_details": true, "show_next": true, "show_playing": true, "show_stats": true,
"confirm_emergency": false}` (the defaults; `mode` can be `list`). Set it in
Settings → Panel, or with `PUT /settings` (`"panel"`, replaced whole).

**Playlists.** `[{"id", "name", "items", "gap_ms"}]`: sounds and text pages
that play one after another, as one stream, with the announcement's chimes
before and after. `items` (up to 100) are `{"type": "sound", "media":
"NAME"}` or `{"type": "text", "text": "...", "speak": true, "show": true,
"voice": "NAME"}` (a text page, up to 2000 characters, spoken, shown or
both: see *Text pages*; `voice` unset is the server's default). `gap_ms` is
the silence between the items heard (default 0, up to 60000). Files at
another sample rate or channel count than the first are converted to it. A
playlist an announcement uses can't be deleted, or left with nothing heard
(a sound or a spoken page). They need an mcast server that plays a list of
files (0.5.5 or later).

```json
"playlists": [{"id": "closing", "name": "Closing", "gap_ms": 500, "items": [
  {"type": "sound", "media": "en/closing.wav"},
  {"type": "text", "text": "The library closes in ten minutes."},
  {"type": "text", "text": "Please return books to the front desk.", "speak": false}]}],
"announcements": [{"id": "closing", "name": "Closing time", "audio": {"playlist": "closing"},
  "zones": ["building-a"],
  "parts": [{"zones": ["building-b"], "audio": {"media": "es/closing.wav"}}]}]
```

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
`volume_db`, `input_gain_db`, `normalize` and `lead_in_ms` when they leave them out, so one chime can be
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

### Who heard it

A speaker running `mcast receive --name Lobby` (or `mcast audio recv -n
Lobby`) sends RTCP receiver reports naming the stream it hears.
`mcast serve`'s monitor collects them, and the panel reads them from it.

When a zone lists its `listeners`, every play to it is checked:
- **When:** while the play goes on, and for up to 12 seconds after. Receivers
  report every 5 seconds or so, and keep naming the last stream they heard.
  The check ends early once everyone expected has been heard.
- **Expected:** the listeners of the zones whose groups the play reached. A
  zone that was busy or failed doesn't expect anything.
- **Heard:** a receiver whose reports named one of the play's streams. That
  includes receivers no zone lists. The worst loss each reported is kept.
- **Silent:** expected but not heard.

The run's `listeners` holds `{"checking", "expected", "heard": [{"name",
"loss_percent"}], "silent", "error"}`, and each target its `expected` and
`heard` names.
- **Where it shows:** run cards (History, and the Panel while playing) say
  "Heard by 3 of 4", and name the silent ones.
- **Toast and webhook:** a silent speaker gets a toast and the
  `listener_silent` webhook.
- **The Monitor tab** has a Speakers table: plays expected, heard and missed,
  and the last one heard.
- **History:** a checked run is written once the check ends, so it's there
  with who heard it.

If the mcast server runs with `--no-monitor`, or can't be asked, the run's
`listeners.error` says so and nobody is called silent. Plays to zones that
list no listeners aren't checked.

### Speakers going offline

Between plays, receivers keep sending reports every few seconds, so the
panel can tell when an expected speaker stops.
- **How it checks:** it asks the monitor every 10 seconds who is reporting
  (`GET /speakers`).
- **Offline:** a speaker that hasn't reported for a minute. It's unplugged,
  switched off, off the network, or not running `mcast receive`.
- **What happens then:**
  - a `listener_offline` webhook;
  - a toast;
  - "Speakers: Office offline" on the Panel's stats strip;
  - a red dot in the Zones list, with how long it's been gone.
- **When it reports again:** `listener_online` and another toast.
- **Not known:** while the monitor is off or the mcast server is
  unreachable, nobody is called offline. The `backend_down` webhook covers
  the mcast server being away.

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
TOKEN` when the panel has a token. Browsers can't send that header everywhere,
so `?access_token=TOKEN` also works on `GET /events`, `GET /media/{path}`,
`GET /chimes/{name}/preview` and `GET /live/{id}`, though not with trigger
tokens. Errors are `{"error": "..."}`: `401` for a missing, wrong or expired
token, `403` for a token whose [role](#access-tokens) can't do that, `404`
for something that isn't there, `409` for a duplicate id, or a zone or
announcement still in use, `422` for a config that wouldn't be valid, `502`
when the mcast server can't be reached.

A trigger token may only play its announcements, and `GET /runs` shows it
only their runs.

| Route | |
|---|---|
| `GET /health` | `{"ok": true, "version", "name", "role", "announcements", "categories"}`: who the token is (`name` is `null` without one) |
| `GET /backend` | The mcast server: `{"url", "connected", "error", "since_ms"}` |
| `GET /config` | The whole config, as in `intercom.json` |
| `GET`, `POST /zones` | List, or add one (without `id`, one is made from `name`) |
| `GET`, `PUT`, `DELETE /zones/{id}` | `PUT` replaces it; the id can't change |
| `GET`, `POST /announcements`, `GET`, `PUT`, `DELETE /announcements/{id}` | The same |
| `GET`, `POST /schedules`, `GET`, `PUT`, `DELETE /schedules/{id}` | The same; without `id` or `name`, the id comes from the announcement |
| `GET`, `POST /sites`, `GET`, `PUT`, `DELETE /sites/{id}` | The same; a site with zones or schedules can't be deleted (`409`) |
| `GET`, `POST /schedule-groups`, `GET`, `PUT`, `DELETE /schedule-groups/{id}` | The same; switch one with `PUT` and `"enabled"`. One with schedules can't be deleted |
| `GET`, `POST /categories`, `GET`, `PUT`, `DELETE /categories/{id}` | As zones; one with announcements in it can't be deleted (`409`) |
| `GET`, `POST /playlists`, `GET`, `PUT`, `DELETE /playlists/{id}` | As zones; one an announcement plays can't be deleted (`409`) |
| `GET /panel-layout`, `PUT /panel-layout` | The Panel's groups and buttons in one: `{"revision", "groups": [{"id", "name", "color", "size", "announcements": [ids]}], "other": [ids]}`, in the Panel's order (`GET`: anyone who can read the config). `PUT` (admins) saves it all at once: groups without an `id` are made (named after them), groups left out are deleted, and announcements move to the group they're listed in (or none, in `other`; ones left out stay put). Its categories, their order and the buttons' order change together, or not at all. With the `revision` it started from, it's refused (409) if the Panel changed meanwhile. Returns the new layout |
| `PUT /layout/{tab}`, `DELETE /layout/{tab}` | Admins: `{"order": [section ids]}` arranges a tab's sections for everyone (kept in `intercom.json` as `layout`; sections left out follow in their usual order); `DELETE` goes back to the usual order. Returns the whole layout. So far the Settings tab (`settings`: `panel`, `defaults`, `time-zone`, `sites`, `chimes`, `tokens`, `other-systems`, `connection`), and the Panel: `panel` (category ids) and `panel-buttons` (announcement ids) |
| `GET /history` | Plays that ended, newest first: `{"runs": [RUN...], "more", "next"}`. Filters: `from`, `to` (dates in the panel's time zone, both included), `announcement`, `state` (`done`, `stopped`, `failed`, `missed`), `by` (part of a token's name), `q` (part of the name), `limit` (default 100, up to 1000), `before` (the `next` of the page before, `started_ms:id`, for the next page). A token limited to categories sees only theirs |
| `GET /stats` | The Panel's stats strip, from the history: `{"today": COUNTS, "week": COUNTS, "last_24h": {"busy_groups", "failed_groups"}}`, with COUNTS `{"plays", "played", "stopped", "failed", "missed"}`; today and the last 7 days are in the panel's time zone. A token limited to categories counts only theirs |
| `GET /monitor?days=7[&site=ID]` | The Monitor tab (1 to 366 days, today included, in the panel's time zone; with `site`, only plays at that site, counting only its zones and speakers; 404 for an unknown site): `{"totals", "days": [{"date", ...COUNTS}], "announcements": [{"id", "name", ...COUNTS}], "zones": [{"id", "name", "sends", "played", "busy", "failed", "taken_over"}], "schedules": {"on_time", "late", "missed", "late_after_ms", "most_late_ms"}, "triggers": {TYPE: n}, "by": [{"name", "plays"}], "listeners": {"checked", "with_silent", "list": [{"name", "expected", "heard", "missed", "last_heard_ms", "worst_loss_percent"}]}, "connection": {"known_ms", "up_ms", "uptime", "outages": [{"from_ms", "to_ms"}]}, "from_ms", "to_ms"}`. Zones are the ones each group is in now. Uptime counts only time the panel was running, from `history/connection.jsonl`. A token limited to categories sees only theirs |
| `GET /history.csv` | The same as a spreadsheet: started, ended (in the panel's time zone), announcement, name, state, trigger, by, groups, played, busy, failed, error |
| `GET /upcoming?n=10&site=ID` | `{"time_zone", "paused", "now_ms", "fires": [{"at_ms", "local", "weekday", "schedule", "announcement", "name", "site"}], "next": {SCHEDULE: at_ms or null}, "sites": {ID: {"time_zone", "paused"}}}`. `site` limits it to one site's schedules (empty for those with none). `local` is in the schedule's own time zone. `fires` leaves out days off and schedules that can't play; `next` leaves out days off only |
| `GET`, `PUT /settings` | `{"time_zone", "defaults", "schedules_paused", "days_off", "panel", "history_days"}`, as in the config (`time_zone` `null` for the system's). `PUT` changes the fields given and leaves the rest; `defaults` and `days_off` are replaced whole. `GET` adds `effective_time_zone` |
| `POST /announcements/{id}/play` | Play it now; returns the run (`201`). Not for live announcements (`422`) |
| `GET /live/{id}` | A WebSocket that talks on live announcement `{id}`: it plays the announcement, then passes each binary message (16-bit little-endian mono PCM at 48 kHz, best in 20 ms chunks) on to every group. Text messages from the panel: `{"type": "started", "run", "groups", "rate"}`, and `{"type": "ended", "reason"}` when it was stopped or couldn't start. Send `end` (text) or close the socket to end the page. Only the panel's own page may open it (`Origin` must match) |
| `GET /runs`, `GET /runs/{id}` | Runs playing and recently ended, newest first |
| `DELETE /runs/{id}` | Stop one; `DELETE /runs` stops them all |
| `GET`, `POST /inputs`; `GET`, `PUT`, `DELETE /inputs/{id}` | GPIO inputs (admin), as in the config |
| `GET /input-status` | Each input's state now (admin): `[{"id", "name", "enabled", "pressed", "error", "changed_ms", "run"}]`; the `inputs` event sends the same when one changes |
| `POST /inputs/{id}/test` | Press it from the API (admin), as if by hand, and for a `while_held` one let go a second later: `{"run"}` |
| `GET /backup` | The config as `intercom.json` (admin); `?sounds=1`: a `.tar.gz` with every sound on the mcast server too (`mcast-intercom-backup.json`, `intercom.json`, `media/PATH`). Never tokens, webhooks or history |
| `POST /restore` | Put a backup back (admin): the raw body is a `.tar.gz` or an `intercom.json`. `?dry_run=1` only says what would change. Returns `{"plan": {"lists": {LIST: {"added", "changed", "removed"}}, "settings", "sounds_in_backup", "upload", "missing", "same"}, "applied", "uploaded", "kept", "server_error"}`; the config before is kept as `intercom.json.bak` |
| `GET /speakers` | Whether each speaker zones expect is reporting between plays: `{"known", "error", "speakers": [{"name", "zones", "online", "since_ms", "last_seen_ms"}]}`. `online` is `null` until known (a minute for one never seen). The `speakers` event sends the same, with `changes: [{"speaker", "online"}]` |
| `GET /display?zones=a,b` | The text pages up on displays now, newest first (with `zones`, those shown in any of them): `{"pages": [{"run", "announcement", "name", "zones", "text", "at_ms", "until_ms"}]}`; `until_ms` is `null` while the run plays. The `display` event sends each page as it goes up and as it's given its end. A token limited to categories sees only theirs |
| `GET /voices` | The mcast server's text to speech: `{"engine", "voices", "default_voice"}` (`engine` is `piper`, `espeak-ng`, `command` or `null`) |
| `GET /listeners` | The receivers `mcast serve`'s monitor hears now (`[{"group", "name", "cname", "about", "loss_percent", "last_heard_ms", "left"}]`), for choosing a zone's listeners; 404 if its monitor is off |
| `GET /media`, `GET /phonebook` | From the mcast server, for choosing audio and groups: media names are paths (`bells/lunch.wav`) |
| `GET /library` | The Sounds tab: `{"files": [{"name", "bytes", "modified_ms", "hidden", "used_by", "audio", "audio_error", "tags"}], "folders": [{"name", "hidden"}], "tags": [{"tag", "files"}]}`; `used_by` names the announcements (and "the defaults") that use a file; `audio` is `{"codec", "rate", "channels", "duration_s"}`, or `audio_error` says why the mcast server can't play it (`null` both with an mcast server before 0.5.5) |
| `GET`, `PUT`, `DELETE /media/{path}` | Listen to, upload (raw body) or delete a file on the mcast server. An upload must be audio the mcast server can play (`422` if not, and any old file stays); it replaces a file of that name, so announcements using it play the new one. Returns `{"name", "bytes", "audio"}` (`201`). `DELETE` is refused (`409`) while something uses it |
| `POST /media-folders`, `DELETE /media-folders/{path}` | `{"name"}`: make a folder; delete an empty one |
| `POST /media-move` | `{"from", "to"}`: rename or move a file or folder, then point what used it at the new name. Returns `{"from", "to", "folder", "updated"}` |
| `PUT /media-hidden` | `{"name", "hidden"}`: hide a file or folder from the lists, or show it again |
| `PUT /media-tags` | `{"names", "add", "remove"}`: tag or untag files, many at once (tags are up to 32 characters, no commas, and match without regard to case). Returns each file's tags: `{NAME: [tags]}`. Tags are kept in `intercom.json` (`media_tags`), follow a file when it's moved, and go when it's deleted |
| `GET /chimes`, `PUT /chimes`, `GET /chimes/{name}/preview` | The mcast server's built-in chimes: list them, hide some from the lists (`{"hidden": [...]}`, kept on the mcast server), and a WAV to listen to |
| `GET /events` | Server-sent events: `run` (`{"run"}`), `config_changed` (also when tokens change), `backend` (`{"status"}`), `media_changed`, `display` (`{"page"}`), `lagged` |
| `GET`, `POST /trigger/{id or name}` | Play it now (`201`, the run), as another system. Overrides for this play: `zones` (some of its zones) and `repeat`, in the query or a JSON body. `GET` takes a trigger token in the URL; `HEAD` and other methods get `405`. Not for live announcements (`422`) |
| `GET`, `POST /trigger/{id or name}/stop` | Stop it wherever it's playing; the runs stopped |
| `GET`, `POST /stop-all` | Stop everything the token may stop; the runs stopped |
| `GET`, `POST /webhooks` | Outbound webhooks (admin): list them (`[{"id", "name", "url", "events", "enabled", "sites", "signed", "created_ms", "last_delivery"}]`), or add one (`{"name", "url", "events", "enabled", "sites", "sign"}`; `sites` limits it to plays and speakers at those sites; returns `{"webhook", "secret"}`, the only time the secret is shown) |
| `GET /webhooks/{id}/deliveries` | Its last 50 deliveries (admin), newest first: `[{"delivery", "event", "about", "at_ms", "ok", "status", "error", "attempts", "took_ms"}]` |
| `PUT`, `DELETE /webhooks/{id}`, `POST /webhooks/{id}/test` | Change one (`{"name", "url", "events", "enabled", "sites"}`; `"sites": []` is every site again), delete it, or send it a `test` event and get how it went: `{"event", "at_ms", "ok", "status", "error", "attempts", "took_ms"}` |
| `GET /tokens` | Access tokens (admin): `[{"id", "name", "role", "announcements", "created_ms", "expires", "expires_ms", "expired", "last_used_ms", "uses"}]`, never their secrets |
| `POST /tokens` | `{"name", "role", "announcements", "categories", "expires"}` (`announcements`, ids or names, for trigger tokens only; `categories`, ids or names, for operator tokens only; `expires` a date like `2027-06-30`): make one. Returns `{"token", "secret"}` (`201`), the only time the secret is shown |
| `PUT /tokens/{id}` | `{"name", "announcements", "categories", "expires"}`: change those given (`"expires": null` clears it). The role can't change |
| `DELETE /tokens/{id}` | Revoke it; returns it |

A run:

```json
{"id": 3, "announcement": "lunch", "name": "Lunch bell", "priority": "normal",
 "trigger": {"type": "manual"}, "state": "playing", "round": 1, "rounds": 2,
 "started_ms": 1794022911066, "ended_ms": null, "error": null,
 "targets": [{"group": "zone-1", "address": "239.192.2.1:10000", "state": "playing",
              "job": 12, "error": null, "played": 0}]}
```

A target's `part` says which of the announcement's parts it plays (from 1;
missing for its own audio). A run that showed text pages has `shown`:
`[{"part", "round", "item", "text", "at_ms"}]`, each with the item heard
(counting only items heard) it came up with. A run's `trigger` says what started it: `{"type": "manual", "by"}` for a
button, push to talk or the command line, `{"type": "api", "by"}` for the
trigger API (or a trigger token on the play route), and `{"type": "schedule", "id", "at_ms"}` for a schedule, with the time
it was due. `by` is the token's name (`"startup token"` for the startup
token), or missing without one. Run `state` is `playing`, `waiting` (between repeats), `stopping`, `done`,
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
| `server/` | `mcast intercom serve`: the API (`mod.rs`, `routes.rs`, `library.rs` for Sounds), push to talk (`live.rs`), HTTPS (`tls.rs`) and the web page (`web.rs`) |
| `assets/` | The web page: no framework and no build step, one module per tab (`panel.js`, `history.js`, `schedules.js`, `announcements.js`, `zones.js`, `sounds.js`, `settings.js`); `talk.js` and `mic-worklet.js` are push to talk, and `chimes.js` for the chime fields and lists; `api.js` is the only one that calls the server |

`cargo test -p mcast-intercom` runs the runner and the API against a fake
backend and, in `tests/server.rs`, against a real in-process `mcast serve`.

As in mcast-web, page text goes in with `h()`, never `innerHTML`, and styles
go in `app.css` or through `h(..., { style })`, because the Content-Security-Policy
forbids inline ones.
