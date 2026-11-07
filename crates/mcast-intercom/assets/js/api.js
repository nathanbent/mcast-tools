// The only module that talks to the panel's server.

const PREFIX = "/api/intercom/v1";
const TOKEN_KEY = "mcast-intercom.token";

export function token() {
  try { return localStorage.getItem(TOKEN_KEY) || ""; } catch { return ""; }
}

export function setToken(t) {
  try { t ? localStorage.setItem(TOKEN_KEY, t) : localStorage.removeItem(TOKEN_KEY); } catch {}
}

export class ApiError extends Error {
  constructor(status, body) {
    super(body?.error || `HTTP ${status}`);
    this.status = status;
  }
}

async function parse(res) {
  const text = await res.text();
  let body = null;
  try { body = text ? JSON.parse(text) : null; } catch { body = { error: text }; }
  if (!res.ok) throw new ApiError(res.status, body);
  return body;
}

export async function api(method, path, body) {
  const t = token();
  const init = { method, headers: t ? { Authorization: `Bearer ${t}` } : {} };
  if (body !== undefined) {
    init.headers["Content-Type"] = "application/json";
    init.body = JSON.stringify(body);
  }
  let res;
  try {
    res = await fetch(`${PREFIX}${path}`, init);
  } catch (e) {
    throw new ApiError(0, { error: `can't reach the panel: ${e.message}` });
  }
  return parse(res);
}

const EVENT_TYPES = ["run", "config_changed", "backend", "lagged"];

// Follow server-sent events. `onStatus` gets "live" (refetch then: events may
// have been missed) or "offline". Reconnects on its own.
export function follow(onEvent, onStatus) {
  let source = null;
  let retry = null;
  const connect = () => {
    const t = token();
    source = new EventSource(`${PREFIX}/events${t ? `?access_token=${encodeURIComponent(t)}` : ""}`);
    source.onopen = () => onStatus("live");
    source.onerror = () => {
      onStatus("offline");
      // The browser retries by itself unless the server refused (e.g. 401).
      if (source.readyState === EventSource.CLOSED) {
        source = null;
        retry = setTimeout(connect, 3000);
      }
    };
    for (const type of EVENT_TYPES) {
      source.addEventListener(type, (e) => onEvent(JSON.parse(e.data)));
    }
  };
  connect();
  return () => { clearTimeout(retry); source?.close(); };
}
