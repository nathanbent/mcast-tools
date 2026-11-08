# Intercom: the trigger API and webhooks

Status: built (roadmap item 2 of v0.5.5, `intercom-roadmap.md`). The user
guide is in `crates/mcast-intercom/README.md` (From other systems). This page
records the decisions made while building it, and has examples for
receivers.

## Trigger API

```
GET|POST  /api/intercom/v1/trigger/{id or name}        play it      -> 201 + the run
GET|POST  /api/intercom/v1/trigger/{id or name}/stop   stop it      -> the runs stopped
GET|POST  /api/intercom/v1/stop-all                    stop all the caller may stop
```

- **Who**: `trigger` tokens (only their announcements, if limited),
  `operator` and `admin`. Runs record `{"type": "api", "by": TOKEN NAME}`.
- **Names**: an id, or a name in any case (`/trigger/Lunch%20bell`).
- **GET** is for devices that can only fetch a URL. A trigger token may come
  as `?access_token=`; other roles must use the header. **HEAD** and other
  methods get `405` and play nothing, so a link preview fetching the URL
  can't page anyone. A GET still changes things: keep these URLs out of
  places that crawl or prefetch links.
- **Overrides** for one play, not saved: `zones` (ids or names) and
  `repeat`, as query parameters (`?zones=hall,yard&repeat=2`) or a JSON body
  (`{"zones": ["hall"], "repeat": 2}`, which wins). `zones` must be some of the
  announcement's own zones. This narrows a play (ring only reception) but
  can't send an announcement somewhere it wasn't set up to go. Any other
  field in the body is a `422`.
- **Live** announcements can't be triggered (`422`): they need someone
  talking.
- `/stop-all` for a limited trigger token stops only its announcements. For
  admin and operator it's the same as `DELETE /runs`.

`POST /announcements/{id}/play` stays, for the page. It takes ids only, and
records `manual` (or `api` for trigger tokens).

## Webhooks

The panel POSTs JSON to each enabled hook that wants the event:

| Event | When | Extra fields |
|---|---|---|
| `run_started` | An announcement started playing | `run` |
| `run_ended` | It finished, or was stopped | `run` |
| `run_failed` | It couldn't play (all groups busy or failed, or the mcast server unreachable) | `run` |
| `schedule_missed` | A schedule's time passed while it couldn't play | `run` |
| `listener_silent` | A speaker its zones expect didn't report hearing it: `run.listeners.silent` names them. Sent once the check ends, up to 12 s after the run | `run` |
| `listener_offline` | Between plays, a speaker zones expect hasn't reported for a minute | `speaker`: `{"name", "zones", "online", "since_ms", "last_seen_ms"}` |
| `listener_online` | It's reporting again | `speaker` |
| `backend_down` | The mcast server became unreachable (or was at start-up) | `status` |
| `backend_up` | It's back | `status` |
| `test` | The Test button | `message` |

Each body has `event`, `at_ms` and `delivery` (a number, new for each
delivery), plus the extra fields. A run is as in `GET /runs`. Each run gets
one `run_started` and one ending event (`run_ended`, `run_failed` or
`schedule_missed`); a run that fails before starting gets only `run_failed`.
A run to zones that list listeners can also get one `listener_silent`.

**Sites.** With sites set up, a hook can be limited to some of them
(`"sites": ["north"]`, or tick them in Settings → Other systems).
- **What it gets:** run events for plays that reached a zone at one of
  those sites, and speaker events for speakers expected by such a zone.
- **What it misses:** plays to zones at no site.
- **Always sent:** `backend_down` and `backend_up`, which are about the
  panel itself.
- **Where it shows:** each run has `sites`: its zones' sites, or a
  schedule's own site. Each speaker has `sites` too.

Headers: `Content-Type: application/json`, `User-Agent:
mcast-intercom/VERSION`, `X-Mcast-Event`, `X-Mcast-Delivery`,
`X-Mcast-Timestamp` (ms), and for signed hooks `X-Mcast-Signature: sha256=HEX`.
HEX is the HMAC-SHA256, keyed with the hook's secret (`whsec_...`, shown
once), of the timestamp, a `.`, and the raw body. Checking the timestamp is
recent stops a recorded delivery being replayed later.

Delivery: a 5 s timeout, no redirects followed, and one retry 3 s later on a
network error, a 5xx or a 429. A 4xx isn't retried. At most 32 deliveries are
under way at once; more are dropped and recorded as such. Each hook's last
delivery (event, time, HTTP status or error, attempts) shows in Settings →
Other systems; it's kept in memory only. Deliveries run on their own
threads, so a slow receiver never holds up a page.

Hooks are kept in `webhooks.json` beside `intercom.json`, readable by its
owner only (it holds the signing secrets), so a copy of the config never
carries them. Admin only: `GET`/`POST /webhooks`, `PUT`/`DELETE
/webhooks/{id}`, `POST /webhooks/{id}/test`. The panel refuses to start with
a broken `webhooks.json`, and notices changes to it while running.

**Delivery log.** Each hook keeps its last 50 deliveries, newest first, in
`GET /webhooks/{id}/deliveries` (admin) and behind each hook's *Log* button.
They're in the order they were sent (by `at_ms`, then `delivery`), even when
a slow receiver or a retry makes one finish after a later one.
- **Each one has:** `delivery` (the `X-Mcast-Delivery` number), `event`,
  `about` (the announcement or speaker), `at_ms`, `ok`, `status` (the
  receiver's HTTP status), `error`, `attempts` and `took_ms`.
- **Where it's kept:** in `webhook-log.jsonl` beside `webhooks.json`, owner
  only, so it outlives a restart. The file is cut back once it passes 5000
  lines, and a deleted hook's deliveries go with it.
- **What it never holds:** the bodies sent, and so no secrets.

Not done (yet): retries beyond one, a delivery log, filtering by
announcement or zone, and sending to a hook only for some sites.

## Examples

### Home Assistant: play from an automation

```yaml
# configuration.yaml
rest_command:
  intercom_doorbell:
    url: "https://pager.local:8651/api/intercom/v1/trigger/doorbell"
    method: POST
    headers:
      Authorization: "Bearer mci_..."   # a trigger token
    verify_ssl: false                   # the Pi's certificate is self-signed
```

Then call `rest_command.intercom_doorbell` from an automation. To narrow it,
add `payload: '{"zones": ["reception"]}'` and `content_type:
application/json`.

### Node-RED

An *http request* node: method `POST`, URL
`https://pager.local:8651/api/intercom/v1/trigger/doorbell`, *Use
authentication* → bearer, with the trigger token (and, for the self-signed
certificate, a TLS config that doesn't verify it).

### A basic IP button or a PBX

Anything that can fetch a URL:

```
https://pager.local:8651/api/intercom/v1/trigger/doorbell?access_token=mci_...
```

Use a trigger token limited to that announcement, since the URL may end up
in the device's logs.

### Checking a signature (Python)

```python
import hashlib, hmac, time

def verify(secret: str, headers, body: bytes) -> bool:
    ts = headers["X-Mcast-Timestamp"]
    if abs(time.time() * 1000 - int(ts)) > 5 * 60 * 1000:
        return False  # too old: maybe replayed
    mac = hmac.new(secret.encode(), ts.encode() + b"." + body, hashlib.sha256)
    return hmac.compare_digest("sha256=" + mac.hexdigest(), headers["X-Mcast-Signature"])
```
