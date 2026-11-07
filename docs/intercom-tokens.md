# Intercom: scoped access tokens (design)

Status: built (roadmap item 1 of v0.5.5, `intercom-roadmap.md`). The user
guide is in `crates/mcast-intercom/README.md` (Access tokens).

Today the panel has one all-powerful bearer token (`--panel-token` /
`MCAST_INTERCOM_TOKEN`), or none at all on loopback. This adds named tokens,
each with a fixed role, so a doorbell, a recording system or a wall tablet
gets only what it needs.

## Roles

One role per token.

| Role | For | Can |
|---|---|---|
| `admin` | People who set the panel up | Everything, including managing tokens |
| `operator` | A wall tablet, a reception desk | Use the Panel: read everything, play, stop, push to talk, history. No editing |
| `trigger` | Other systems (webhooks, buttons, PBX) | Play announcements and stop the runs they're allowed to; read a run's state |
| `media` | A recording or upload script | The Sounds tab's operations: list, upload, download, delete, folders, move, hide |

A `trigger` token can carry an optional **announcement list**. Empty means
any announcement. Otherwise it may play only those, and stop only runs of
those.

### Route access

| Routes | admin | operator | trigger | media |
|---|---|---|---|---|
| `GET /health` (now also returns the caller's `name`, `role`) | ✓ | ✓ | ✓ | ✓ |
| `GET` zones, announcements, schedules, sites, schedule groups, settings, upcoming, config, backend, phonebook, chimes, chime previews | ✓ | ✓ | — | — |
| `GET /events` | ✓ | ✓ | — | ✓² |
| `POST /announcements/{id}/play`, `GET /runs`, `GET /runs/{id}`, `DELETE /runs/{id}` | ✓ | ✓ | limited¹ | — |
| `DELETE /runs` (stop all), `GET /live/{id}` (push to talk) | ✓ | ✓ | — | — |
| `GET /media`, `/library`, `/media/{name}`; `PUT`/`DELETE /media/{name}`; media folders, move, hidden | ✓ | read only | — | ✓ |
| Create, change or delete zones, announcements, schedules, sites, schedule groups, settings, chimes | ✓ | — | — | — |
| `/tokens` (below) | ✓ | — | — | — |

¹ Only allowed announcements, and only runs of those. `GET /runs` lists
only those runs.

² Added while building: the Sounds tab follows `media_changed` events to
stay current.

A valid token without the role gets `403` ("this token (doorbell, trigger)
can't change zones"), not `401`, so the page and scripts can tell the two
apart.

The trigger API from roadmap item 2 (`/trigger/{name-or-id}`) follows the
`trigger` row when it's built.

### Tokens in the URL

Browsers can't set headers on EventSource, media links or WebSockets, so
`?access_token=` stays. It's narrowed, though:

- **admin, operator, media**: only on `GET /events`, `GET /media/{name}`,
  `GET /chimes/{name}/preview` and `GET /live/{id}`. These are exactly what
  the page needs.
- **trigger**: only on the `GET` form of the trigger API (roadmap item 2),
  for devices that can only fetch a URL. At worst, a token leaked through a
  log can fire its allowed announcements. (Built: `/trigger/...` and
  `/stop-all`, see `intercom-triggers.md`.)

A token in the URL where it isn't allowed gets `403`.

On an open panel, unknown or expired tokens are ignored (the request acts as
admin, as before), so a stale token left in a browser doesn't lock anyone out.

## Tokens

- Format: `mci_` + 32 random bytes, base64url (47 characters). The prefix
  makes them easy to spot in logs and secret scanners. They're generated
  with `ring`'s `SystemRandom`. `ring` is already in the tree through rustls,
  so no new dependencies.
- Only the SHA-256 of the token is stored. The token is shown once, at
  creation. If it's lost, revoke it and make a new one. A slow hash isn't
  needed: the tokens are random, not passwords. Lookup goes by hash, so
  there's no timing leak on the secret.
- Fields: `id`, `name` (unique, one line), `role`, `announcements` (trigger
  only), `created_ms`, `expires_ms` (optional), `last_used_ms`, `uses`.
- Expiry: the page picks a date. The token stops working at the end of that
  day in the panel's time zone. Expired tokens stay listed (marked expired)
  until revoked.
- `last_used_ms` and `uses` are kept in memory and written at most once a
  minute and at shutdown, so the Pi's SD card isn't written on every
  request.

### Storage: `tokens.json`

- It lives beside `intercom.json`, at `<dir of --config>/tokens.json`, or
  `--tokens PATH` / `MCAST_INTERCOM_TOKENS`.
- Written atomically with mode `0600`. It's kept out of `intercom.json` so a
  config backup or export (roadmap item 13) never carries the hashes.
- The server re-reads it when its modification time changes, so the CLI can
  add or revoke tokens while the panel runs. Revocation takes effect on the
  next request.

```jsonc
{ "tokens": [{
    "id": "t_8f2c", "name": "Doorbell", "role": "trigger",
    "announcements": ["a_doorbell"],
    "hash": "sha256:…", "created_ms": 0, "expires_ms": null,
    "last_used_ms": null, "uses": 0
}] }
```

### The startup token and open mode

- `--panel-token` stays, and acts as **admin**, named "startup token" in
  history. It isn't in `tokens.json` and can't be revoked from the page:
  change it where it's set (`pi.conf` / the env file).
- Listening beyond loopback still needs some admin credential: either
  `--panel-token`, or at least one unexpired `admin` token in `tokens.json`.
  Otherwise the panel refuses to start, as today.
- On loopback with no `--panel-token`, requests without a token are still
  treated as admin (unchanged). Requests that do carry a token are checked
  and get that token's role, so a local script can test a trigger token.

## API: `/api/intercom/v1/tokens` (admin)

```
GET    /tokens          -> [Token]      (never the secret or hash)
POST   /tokens          {name, role, announcements?, expires?}
                        -> 201 {token: Token, secret: "mci_…"}   (only time it's shown)
PUT    /tokens/{id}     {name?, announcements?, expires?}   (role can't change: make a new one)
DELETE /tokens/{id}     -> the revoked Token
```

Changes send `config_changed`, so other open pages refresh.

## CLI

```
mcast intercom token add NAME --role trigger [--announcement ID|NAME]... [--expires 2027-01-31]
mcast intercom token list
mcast intercom token revoke NAME|ID
```

They take `--config` / `--tokens` like `serve`, and edit `tokens.json`
directly. A running panel picks the change up. `add` prints the secret
once.

## History

`Trigger` gains who pressed or called. Button presses through an operator
token record the token's name, a trigger token records its name, the
startup token records "startup token", and open mode records nothing.

```rust
pub enum Trigger {
    Manual { by: Option<String> },   // button, CLI, push to talk
    Api { by: String },              // trigger-role token
    Schedule { id: String, at_ms: u64 },
}
```

The History tab shows "by Doorbell". When history is persisted (roadmap
item 6), this goes into the log as well.

## Web page

- After login, the page reads `role` from `/health` and shows only what the
  role allows:
  - **operator**: Panel and History, plus push to talk. Sounds is read-only
    at most; the editing tabs are hidden.
  - **media**: the Sounds tab only.
  - **trigger**: a message that this token is for other systems, not the
    page.
- **Settings → Access tokens** (admin):
  - The list shows name, role, allowed announcements, created, last used,
    uses and expiry (or "expired").
  - "New token" asks for name, role, announcements (trigger) and expiry,
    then shows the secret once with a copy button and an example `curl`.
  - Revoke asks for confirmation.
- A `403` shows the server's message as a toast. It doesn't bring up the
  login box (that's still only for `401`).

## Pi

- `install.sh` keeps generating `MCAST_INTERCOM_TOKEN` as the startup admin
  token.
- `tokens.json` goes in the same state directory as `intercom.json`, owned
  by the service user.

## Tests

- Role × route matrix: every route is checked against every role, so a new
  route can't slip through unguarded.
- Trigger limits: playing or stopping a disallowed announcement gets `403`,
  and `GET /runs` is filtered.
- Query tokens: refused outside the allowed routes, for each role.
- Expiry, revocation by CLI while serving (file reload), and that the
  secret appears once and never in `GET /tokens` or in `tokens.json`.
- Startup rules: non-loopback with only `tokens.json` admin tokens starts;
  with none, it refuses.

## Order of work

1. `tokens.rs`: model, file store, hashing, reload, usage flush.
2. Auth middleware → a `Caller {name, role, announcements}` request
   extension; a per-route role check; query-token rules; `403`s.
3. `/tokens` API and `Trigger.by`.
4. CLI `token add|list|revoke`.
5. Web page: role-aware tabs and Settings → Access tokens.
6. Docs (`api.md`, intercom README), Pi notes, tests throughout.
