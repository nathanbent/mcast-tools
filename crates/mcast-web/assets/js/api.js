// The only module that talks to the server.

const TOKEN_KEY = "mcast.token";

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
    this.job = body?.job ?? null;
  }
}

function headers(extra = {}) {
  const t = token();
  return t ? { ...extra, Authorization: `Bearer ${t}` } : extra;
}

async function parse(res) {
  const text = await res.text();
  let body = null;
  try { body = text ? JSON.parse(text) : null; } catch { body = { error: text }; }
  if (!res.ok) throw new ApiError(res.status, body);
  return body;
}

export async function api(method, path, body) {
  const init = { method, headers: headers() };
  if (body !== undefined) {
    init.headers["Content-Type"] = "application/json";
    init.body = JSON.stringify(body);
  }
  return parse(await fetch(`/api/v1${path}`, init));
}

// PUT a file to the media directory, reporting progress (0..1).
export function upload(name, file, onProgress) {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open("PUT", `/api/v1/media/${encodeURIComponent(name)}`);
    for (const [k, v] of Object.entries(headers())) xhr.setRequestHeader(k, v);
    xhr.upload.onprogress = (e) => e.lengthComputable && onProgress?.(e.loaded / e.total);
    xhr.onload = () => {
      let body = null;
      try { body = JSON.parse(xhr.responseText); } catch {}
      xhr.status < 300 ? resolve(body) : reject(new ApiError(xhr.status, body));
    };
    xhr.onerror = () => reject(new ApiError(0, { error: "upload failed: network error" }));
    xhr.send(file);
  });
}

// URL of a media file, for <img> and download links (which can't send headers).
export function mediaUrl(name) {
  const t = token();
  return `/api/v1/media/${encodeURIComponent(name)}${t ? `?access_token=${encodeURIComponent(t)}` : ""}`;
}

const EVENT_TYPES = [
  "job_started", "job_updated", "job_ended",
  "stream_added", "stream_changed", "stream_removed",
  "monitor", "text_line", "file_received", "probe_updated", "probe_removed", "lagged",
];

// Follow server-sent events. `onStatus` gets "live" (refetch lists then: events
// may have been missed) or "offline". Reconnects on its own.
export function follow(onEvent, onStatus) {
  let source = null;
  let retry = null;
  const connect = () => {
    const t = token();
    source = new EventSource(`/api/v1/events${t ? `?access_token=${encodeURIComponent(t)}` : ""}`);
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
