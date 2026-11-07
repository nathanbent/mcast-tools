//! File and picture senders and receivers.

use super::{
    invalid, JobId, JobInfo, JobKind, JobRequest, JobState, JobStats, Jobs, Shared, StartError,
};
use crate::request::{FileRecvRequest, FileSendRequest};
use crate::{now_ms, Event};
use anyhow::Context;
use mcast_file::receiver::{self, safe_file_name, Completed, Event as FileEvent};
use mcast_file::sender::FileSender;
use mcast_file::session::FileInfo;
use serde::Serialize;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct FileSendStats {
    /// The file going out now.
    pub file: String,
    /// Its position (from 1) among `count` files.
    pub index: usize,
    pub count: usize,
    /// Packets sent for this file, how many hold the file itself, and how
    /// many it will take (`None` while looping).
    pub sent: u64,
    pub needed: u64,
    pub total: Option<u64>,
    pub looping: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct FileRecvStats {
    /// The file arriving now, and how much of what's needed is in.
    pub receiving: Option<String>,
    pub percent: Option<u8>,
    /// Files saved, and files given up on or refused.
    pub received: u64,
    pub failed: u64,
}

/// A file that arrived and was saved.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReceivedFile {
    /// The name it was saved under (in the media directory).
    pub name: String,
    /// The name its sender gave, if different.
    pub sent_as: Option<String>,
    pub bytes: u64,
    pub content_type: Option<String>,
    /// The checksum matched (`None` if its metadata never arrived).
    pub verified: Option<bool>,
    pub from: SocketAddr,
    pub at_ms: u64,
}

impl Shared {
    fn recv_stats(&self, id: JobId, g: &mut dyn FnMut(&mut FileRecvStats)) {
        self.file_stats(id, |s| {
            if let JobStats::FileRecv(f) = s {
                g(f);
            }
        });
    }

    fn file_stats<T>(&self, id: JobId, f: impl FnOnce(&mut JobStats) -> T) -> Option<T> {
        let mut st = self.lock();
        st.jobs
            .get_mut(&id)
            .and_then(|j| j.info.stats.as_mut())
            .map(f)
    }
}

impl Jobs {
    /// Send files one after another (the last one looping if asked).
    /// `files` are read before anything is sent, so a bad one fails here.
    pub fn start_file_sender(&self, req: FileSendRequest) -> Result<JobInfo, StartError> {
        self.start_file_sender_as(req.clone(), req)
    }

    /// Like [`Jobs::start_file_sender`], listing the job with `shown` as its
    /// request (e.g. the media names a client gave rather than full paths).
    pub fn start_file_sender_as(
        &self,
        req: FileSendRequest,
        shown: FileSendRequest,
    ) -> Result<JobInfo, StartError> {
        let cfg = req.to_config(self.shared.default_iface).map_err(invalid)?;
        let mut files = Vec::new();
        for path in &req.files {
            let data = std::fs::read(path)
                .with_context(|| format!("reading {}", path.display()))
                .map_err(invalid)?;
            if data.is_empty() {
                return Err(StartError::Invalid(format!("{} is empty", path.display())));
            }
            let name = path
                .file_name()
                .map_or_else(|| "file".into(), |n| n.to_string_lossy().into_owned());
            files.push((name, data));
        }
        let stop = Arc::new(AtomicBool::new(false));
        let count = files.len();
        let info = JobInfo {
            id: 0,
            kind: JobKind::FileSend,
            state: JobState::Running,
            group: Some(req.group),
            name: cfg.announce.as_ref().map(|a| a.name.clone()),
            summary: Some(format!(
                "sending {count} file{} to {} at {} kbit/s",
                if count == 1 { "" } else { "s" },
                req.group,
                req.rate_kbps
            )),
            notes: Vec::new(),
            created_ms: now_ms(),
            ended_ms: None,
            error: None,
            stats: Some(JobStats::FileSend(FileSendStats {
                count,
                ..FileSendStats::default()
            })),
            request: JobRequest::FileSend(shown),
        };
        let info = self.shared.register(info, stop.clone(), Some(req.group))?;
        let id = info.id;
        let mut sender = match FileSender::new(cfg) {
            Ok(s) => s,
            Err(e) => {
                self.shared.discard(id);
                return Err(invalid(e));
            }
        };
        let looped = req.looped;
        let shared = self.shared.clone();
        let thread = std::thread::Builder::new()
            .name(format!("mcast-file-send-{id}"))
            .spawn(move || {
                let mut result = Ok(());
                for (i, (name, data)) in files.iter().enumerate() {
                    if stop.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    let last = i + 1 == count;
                    sender.set_looped(last && looped);
                    let status = |sent, needed, total| {
                        JobStats::FileSend(FileSendStats {
                            file: name.clone(),
                            index: i + 1,
                            count,
                            sent,
                            needed,
                            total,
                            looping: last && looped,
                        })
                    };
                    shared.file_stats(id, |s| *s = status(0, 0, None));
                    let sent = sender.send_file(name, data, &stop, |p| {
                        shared.file_stats(id, |s| *s = status(p.sent, p.source, p.total));
                    });
                    match sent {
                        Ok(n) => {
                            shared.file_stats(id, |s| {
                                if let JobStats::FileSend(f) = s {
                                    f.sent = n;
                                }
                            });
                        }
                        Err(e) => {
                            result = Err(e);
                            break;
                        }
                    }
                }
                shared.finish(id, result.map(|_| None));
            });
        match thread {
            Ok(t) => self.shared.attach(id, t),
            Err(e) => {
                self.shared.discard(id);
                return Err(StartError::Invalid(format!("starting thread: {e}")));
            }
        }
        self.shared.emit(Event::JobStarted { job: info.clone() });
        Ok(info)
    }

    /// Receive files into `req.out_dir` until stopped. Announced streams in
    /// `known` can be picked by name.
    pub fn start_file_receiver(
        &self,
        req: FileRecvRequest,
        known: &[FileInfo],
    ) -> Result<JobInfo, StartError> {
        let iface = self.shared.default_iface;
        let (group, pt, announced) = req.target(known).map_err(invalid)?;
        let cfg = req.to_config(group, pt, iface).map_err(invalid)?;
        let out_dir = req.out_dir.clone();
        if !out_dir.is_dir() {
            return Err(StartError::Invalid(format!(
                "{} isn't a directory",
                out_dir.display()
            )));
        }
        let stop = Arc::new(AtomicBool::new(false));
        let info = JobInfo {
            id: 0,
            kind: JobKind::FileRecv,
            state: JobState::Running,
            group: Some(group),
            name: announced,
            summary: Some(format!(
                "receiving files from {group} (up to {} MB each)",
                req.max_size_mb
            )),
            notes: Vec::new(),
            created_ms: now_ms(),
            ended_ms: None,
            error: None,
            stats: Some(JobStats::FileRecv(FileRecvStats::default())),
            request: JobRequest::FileRecv(req),
        };
        let info = self.shared.register(info, stop.clone(), None)?;
        let id = info.id;
        let shared = self.shared.clone();
        let thread = std::thread::Builder::new()
            .name(format!("mcast-file-recv-{id}"))
            .spawn(move || {
                let mut names: HashMap<u32, String> = HashMap::new();
                let mut failure = None;
                let result = receiver::run(cfg, &stop, |e| {
                    let stats = |g: &mut dyn FnMut(&mut FileRecvStats)| shared.recv_stats(id, g);
                    match e {
                        FileEvent::Started { object, name, .. } => {
                            let label = name.clone().unwrap_or_else(|| format!("file {object:08x}"));
                            if let Some(n) = name {
                                names.insert(object, n);
                            }
                            stats(&mut |f| {
                                f.receiving = Some(label.clone());
                                f.percent = Some(0);
                            });
                        }
                        FileEvent::Named { object, name } => {
                            names.insert(object, name.clone());
                            stats(&mut |f| f.receiving = Some(name.clone()));
                        }
                        FileEvent::Progress {
                            received, source, ..
                        } => {
                            let pct = (received * 100 / source.max(1)).min(99) as u8;
                            stats(&mut |f| f.percent = Some(pct));
                        }
                        FileEvent::Completed(c) => match save(&out_dir, &c) {
                            Ok(Some(file)) => {
                                stats(&mut |f| {
                                    f.received += 1;
                                    f.receiving = None;
                                    f.percent = None;
                                });
                                shared.emit(Event::FileReceived { job: id, file });
                            }
                            Ok(None) => {
                                stats(&mut |f| {
                                    f.failed += 1;
                                    f.receiving = None;
                                    f.percent = None;
                                });
                                let name = names.get(&c.object).cloned().unwrap_or_default();
                                shared.update(id, |i| {
                                    i.notes.push(format!("{name}: checksum mismatch, not saved"))
                                });
                            }
                            Err(e) => {
                                failure = Some(e);
                                return false;
                            }
                        },
                        FileEvent::Abandoned { name, received, source, .. } => {
                            stats(&mut |f| {
                                f.failed += 1;
                                f.receiving = None;
                                f.percent = None;
                            });
                            let name = name.unwrap_or_else(|| "a file".into());
                            shared.update(id, |i| {
                                i.notes.push(format!(
                                    "gave up on {name}: the sender stopped at {received} of {source} packets"
                                ))
                            });
                        }
                        FileEvent::Rejected { from, reason } => {
                            shared.update(id, |i| i.notes.push(format!("refused files from {from}: {reason}")));
                        }
                        FileEvent::Warning { message } => {
                            shared.update(id, |i| i.notes.push(message));
                        }
                    }
                    true
                });
                let result = match failure {
                    Some(e) => Err(e),
                    None => result,
                };
                shared.finish(id, result.map(|_| None));
            });
        match thread {
            Ok(t) => self.shared.attach(id, t),
            Err(e) => {
                self.shared.discard(id);
                return Err(StartError::Invalid(format!("starting thread: {e}")));
            }
        }
        self.shared.emit(Event::JobStarted { job: info.clone() });
        Ok(info)
    }
}

/// Save a completed file under a safe, unused name. `Ok(None)` if its
/// checksum failed (not saved).
fn save(dir: &Path, c: &Completed) -> anyhow::Result<Option<ReceivedFile>> {
    if c.verified == Some(false) {
        return Ok(None);
    }
    let sent_as = c.meta.as_ref().map(|m| m.name.clone());
    let base = safe_file_name(sent_as.as_deref().unwrap_or(""), c.object);
    // Written beside it and renamed, so a half-written file never shows.
    let tmp = dir.join(format!(".recv-{:08x}", c.object));
    std::fs::write(&tmp, &c.data).with_context(|| format!("writing {}", tmp.display()))?;
    let path = unique_path(dir, &base);
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(anyhow::Error::from(e).context(format!("saving {}", path.display())));
    }
    let name = path
        .file_name()
        .map_or_else(|| base.clone(), |n| n.to_string_lossy().into_owned());
    Ok(Some(ReceivedFile {
        sent_as: sent_as.filter(|s| *s != name),
        name,
        bytes: c.data.len() as u64,
        content_type: c
            .meta
            .as_ref()
            .map(|m| m.content_type.clone())
            .filter(|t| !t.is_empty()),
        verified: c.verified,
        from: c.from,
        at_ms: now_ms(),
    }))
}

/// `dir/name`, or `dir/stem-2.ext`, `-3`... if taken.
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
        _ => (name, String::new()),
    };
    (2..)
        .map(|n| dir.join(format!("{stem}-{n}{ext}")))
        .find(|p| !p.exists())
        .expect("some name is free")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_names() {
        let dir = std::env::temp_dir().join(format!("mcast-unique-{}", mcast_core::random_u32()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(unique_path(&dir, "a.png"), dir.join("a.png"));
        std::fs::write(dir.join("a.png"), b"x").unwrap();
        std::fs::write(dir.join("a-2.png"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "a.png"), dir.join("a-3.png"));
        std::fs::write(dir.join("notes"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "notes"), dir.join("notes-2"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
