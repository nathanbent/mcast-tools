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

// A media path for a URL: each part encoded, `/` kept.
const mediaPath = (name) => name.split("/").map(encodeURIComponent).join("/");

// URL of a media file, for <audio> (which can't send headers).
export function mediaUrl(name) {
  const t = token();
  return `${PREFIX}/media/${mediaPath(name)}${t ? `?access_token=${encodeURIComponent(t)}` : ""}`;
}

// PUT a file to the mcast server's media, reporting progress (0..1).
export function upload(name, file, onProgress) {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open("PUT", `${PREFIX}/media/${mediaPath(name)}`);
    const t = token();
    if (t) xhr.setRequestHeader("Authorization", `Bearer ${t}`);
    xhr.upload.onprogress = (e) => { if (e.lengthComputable) onProgress?.(e.loaded / e.total); };
    xhr.onload = () => {
      if (xhr.status < 300) return resolve();
      let body = null;
      try { body = JSON.parse(xhr.responseText); } catch {}
      reject(new ApiError(xhr.status, body));
    };
    xhr.onerror = () => reject(new ApiError(0, { error: "can't reach the panel" }));
    xhr.send(file);
  });
}

const EVENT_TYPES = ["run", "config_changed", "backend", "media_changed", "speakers", "lagged"];

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

// Save what `path` returns as a file (with the token in a header, not the URL).
export async function download(path, filename) {
  const t = token();
  const res = await fetch(`${PREFIX}${path}`, { headers: t ? { Authorization: `Bearer ${t}` } : {} });
  if (!res.ok) {
    let body = null;
    try { body = await res.json(); } catch {}
    throw new ApiError(res.status, body);
  }
  const url = URL.createObjectURL(await res.blob());
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  document.body.append(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
