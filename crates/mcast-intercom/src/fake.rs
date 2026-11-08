//! A fake mcast server for tests: jobs that run until told to end, one
//! sender per group, and events delivered synchronously.

use crate::backend::{
    Backend, BackendError, BackendResult, Entry, Feed, FeedFn, Job, Listener, MediaFile,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub(crate) struct FakeState {
    next: u64,
    jobs: HashMap<u64, (String, Job)>,
    requests: Vec<Value>,
    /// Report jobs as ended before replying to the start.
    pub(crate) end_at_once: bool,
    /// Don't send events.
    pub(crate) quiet: bool,
    /// Unreachable: the phonebook and media lists fail.
    pub(crate) down: bool,
    /// Hidden built-in chimes.
    pub(crate) hidden_chimes: Vec<String>,
    /// Media files (by path) and their sizes, once anything has touched
    /// them; until then, just `lunch.wav`.
    files: Option<BTreeMap<String, u64>>,
    folders: BTreeSet<String>,
    /// Live audio streamed into each job, once its stream ended.
    pub(crate) live_bytes: HashMap<u64, usize>,
    /// What the monitor hears; `None` is a server without one (404).
    pub(crate) listeners: Option<Vec<Listener>>,
}

impl FakeState {
    /// Folders stay until deleted, like directories: note those `name` is in.
    fn add_parents(&mut self, name: &str) {
        let mut at = 0;
        while let Some(i) = name[at..].find('/') {
            self.folders.insert(name[..at + i].to_string());
            at += i + 1;
        }
    }

    fn files(&mut self) -> &mut BTreeMap<String, u64> {
        self.files
            .get_or_insert_with(|| BTreeMap::from([("lunch.wav".to_string(), 1000)]))
    }
}

fn err(status: u16, message: String) -> BackendError {
    BackendError {
        status,
        message,
        job: None,
    }
}

/// The fake's built-in chimes.
const CHIMES: [(&str, &str); 3] = [("rising", "chime"), ("bell", "bell"), ("klaxon", "alert")];

#[derive(Default)]
pub(crate) struct Fake {
    pub(crate) st: Mutex<FakeState>,
    feed: Mutex<Option<Arc<FeedFn>>>,
}

impl Fake {
    fn reachable(&self) -> BackendResult<()> {
        if self.st.lock().unwrap().down {
            return Err(BackendError {
                status: 0,
                message: "can't reach the mcast server: down".into(),
                job: None,
            });
        }
        Ok(())
    }

    pub(crate) fn emit(&self, f: Feed) {
        if self.st.lock().unwrap().quiet {
            return;
        }
        let feed = self.feed.lock().unwrap().clone();
        if let Some(feed) = feed {
            feed(f);
        }
    }

    pub(crate) fn end(&self, id: u64, state: &str) {
        let job = {
            let mut st = self.st.lock().unwrap();
            let (_, job) = st.jobs.get_mut(&id).expect("job");
            if job.is_over() {
                return;
            }
            job.state = state.into();
            job.clone()
        };
        self.emit(Feed::JobEnded(job));
    }

    pub(crate) fn running_on(&self, group: &str) -> Option<u64> {
        let st = self.st.lock().unwrap();
        st.jobs
            .iter()
            .find(|(_, (g, j))| g == group && !j.is_over())
            .map(|(id, _)| *id)
    }

    pub(crate) fn finish_all(&self) {
        let ids: Vec<u64> = {
            let st = self.st.lock().unwrap();
            st.jobs
                .iter()
                .filter(|(_, (_, j))| !j.is_over())
                .map(|(id, _)| *id)
                .collect()
        };
        for id in ids {
            self.end(id, "finished");
        }
    }

    pub(crate) fn requests(&self) -> Vec<Value> {
        self.st.lock().unwrap().requests.clone()
    }
}

impl Backend for Fake {
    fn phonebook(&self) -> BackendResult<Vec<Entry>> {
        self.reachable()?;
        Ok(vec![Entry {
            name: "Zone-1".into(),
            group: "239.1.1.1:5000".into(),
            description: String::new(),
        }])
    }

    fn media_files(&self) -> BackendResult<Vec<MediaFile>> {
        self.reachable()?;
        let mut st = self.st.lock().unwrap();
        Ok(st
            .files()
            .iter()
            .map(|(name, bytes)| MediaFile {
                name: name.clone(),
                bytes: *bytes,
                modified_ms: Some(1),
                // Pretend anything but .txt is a second of audio per KB.
                audio: (!name.ends_with(".txt")).then(|| {
                    serde_json::json!({"codec": "pcm_s16le", "rate": 48000, "channels": 1,
                                       "duration_s": *bytes as f64 / 1000.0})
                }),
                audio_error: name.ends_with(".txt").then(|| "no audio track".into()),
            })
            .collect())
    }

    fn media_folders(&self) -> BackendResult<Vec<String>> {
        self.reachable()?;
        Ok(self.st.lock().unwrap().folders.iter().cloned().collect())
    }

    fn upload_media(&self, name: &str, data: &[u8]) -> BackendResult<Value> {
        self.reachable()?;
        // As the server checks: this isn't audio.
        if data.starts_with(b"not audio") {
            return Err(err(
                422,
                format!(
                    "\"{name}\" isn't audio mcast can play: unsupported or unreadable audio file"
                ),
            ));
        }
        let mut st = self.st.lock().unwrap();
        st.add_parents(name);
        st.files().insert(name.into(), data.len() as u64);
        drop(st);
        self.emit(Feed::MediaChanged);
        Ok(serde_json::json!({"name": name, "bytes": data.len(),
            "audio": {"codec": "pcm_s16le", "rate": 48000, "channels": 1, "duration_s": data.len() as f64 / 1000.0}}))
    }

    fn delete_media(&self, name: &str) -> BackendResult<()> {
        self.reachable()?;
        let gone = self.st.lock().unwrap().files().remove(name).is_some();
        if !gone {
            return Err(err(404, format!("no media file \"{name}\"")));
        }
        self.emit(Feed::MediaChanged);
        Ok(())
    }

    fn create_folder(&self, name: &str) -> BackendResult<()> {
        self.reachable()?;
        let mut st = self.st.lock().unwrap();
        st.add_parents(name);
        st.folders.insert(name.into());
        drop(st);
        self.emit(Feed::MediaChanged);
        Ok(())
    }

    fn delete_folder(&self, name: &str) -> BackendResult<()> {
        self.reachable()?;
        let inside = format!("{name}/");
        {
            let mut st = self.st.lock().unwrap();
            let full = st.files().keys().any(|f| f.starts_with(&inside))
                || st.folders.iter().any(|f| f.starts_with(&inside));
            if full {
                return Err(err(409, format!("folder \"{name}\" isn't empty")));
            }
            if !st.folders.remove(name) {
                return Err(err(404, format!("no folder \"{name}\"")));
            }
        }
        self.emit(Feed::MediaChanged);
        Ok(())
    }

    fn move_media(&self, from: &str, to: &str) -> BackendResult<bool> {
        self.reachable()?;
        let folder = {
            let mut st = self.st.lock().unwrap();
            let files = st.files().clone();
            if files.contains_key(to) || st.folders.contains(to) {
                return Err(err(409, format!("\"{to}\" already exists")));
            }
            st.add_parents(to);
            if let Some(bytes) = files.get(from) {
                st.files().remove(from);
                st.files().insert(to.into(), *bytes);
                false
            } else {
                let inside = format!("{from}/");
                let moved: Vec<_> = files
                    .keys()
                    .filter(|f| f.starts_with(&inside))
                    .cloned()
                    .collect();
                if moved.is_empty() && !st.folders.contains(from) {
                    return Err(err(404, format!("no file or folder \"{from}\"")));
                }
                for f in moved {
                    let bytes = st.files().remove(&f).unwrap();
                    st.files()
                        .insert(format!("{to}/{}", &f[inside.len()..]), bytes);
                }
                let folders: Vec<_> = st.folders.iter().cloned().collect();
                for f in folders {
                    if f == from || f.starts_with(&inside) {
                        st.folders.remove(&f);
                        st.folders.insert(format!("{to}{}", &f[from.len()..]));
                    }
                }
                true
            }
        };
        self.emit(Feed::MediaChanged);
        Ok(folder)
    }

    fn download_media(&self, name: &str) -> BackendResult<(String, Vec<u8>)> {
        self.reachable()?;
        match self.st.lock().unwrap().files().get(name) {
            // As long as it says it is.
            Some(&bytes) => {
                let mut data = b"RIFF fake wav".to_vec();
                data.resize(bytes as usize, 0);
                Ok(("audio/wav".into(), data))
            }
            None => Err(err(404, format!("no media file \"{name}\""))),
        }
    }

    fn chimes(&self) -> BackendResult<Vec<Value>> {
        self.reachable()?;
        let st = self.st.lock().unwrap();
        Ok(CHIMES
            .iter()
            .map(|(name, category)| {
                serde_json::json!({
                    "name": name, "category": category, "description": "",
                    "hidden": st.hidden_chimes.iter().any(|h| h == name),
                })
            })
            .collect())
    }

    fn hide_chimes(&self, hidden: &[String]) -> BackendResult<Vec<Value>> {
        self.reachable()?;
        if let Some(bad) = hidden.iter().find(|h| !CHIMES.iter().any(|(n, _)| n == h)) {
            return Err(BackendError {
                status: 422,
                message: format!("unknown chime '{bad}'"),
                job: None,
            });
        }
        self.st.lock().unwrap().hidden_chimes = hidden.to_vec();
        self.chimes()
    }

    fn chime_preview(&self, name: &str) -> BackendResult<Vec<u8>> {
        self.reachable()?;
        if !CHIMES.iter().any(|(n, _)| *n == name) {
            return Err(BackendError {
                status: 404,
                message: format!("unknown chime '{name}'"),
                job: None,
            });
        }
        Ok(b"RIFF fake wav".to_vec())
    }

    fn start_sender(&self, request: &Value) -> BackendResult<Job> {
        let group = request["group"].as_str().unwrap().to_string();
        if let Some(other) = self.running_on(&group) {
            return Err(BackendError {
                status: 409,
                message: format!("{group} is already being sent to by job {other}"),
                job: Some(other),
            });
        }
        let (job, end_at_once) = {
            let mut st = self.st.lock().unwrap();
            st.next += 1;
            let job = Job {
                id: st.next,
                state: "running".into(),
                group: Some(group.clone()),
                error: None,
                ssrc: Some(0x5000_0000 + st.next as u32),
            };
            st.jobs.insert(job.id, (group, job.clone()));
            st.requests.push(request.clone());
            (job, st.end_at_once)
        };
        if end_at_once {
            self.end(job.id, "finished");
        }
        Ok(job)
    }

    fn stop_job(&self, id: u64) -> BackendResult<()> {
        self.end(id, "stopped");
        Ok(())
    }

    fn job(&self, id: u64) -> BackendResult<Job> {
        self.st
            .lock()
            .unwrap()
            .jobs
            .get(&id)
            .map(|(_, j)| j.clone())
            .ok_or(BackendError {
                status: 404,
                message: format!("no job {id}"),
                job: None,
            })
    }

    fn listeners(&self) -> BackendResult<Vec<Listener>> {
        self.reachable()?;
        self.st
            .lock()
            .unwrap()
            .listeners
            .clone()
            .ok_or_else(|| err(404, "the monitor is off (started with --no-monitor)".into()))
    }

    fn stream_live(
        &self,
        job: u64,
        chunks: std::sync::mpsc::Receiver<Vec<u8>>,
    ) -> BackendResult<()> {
        let bytes: usize = chunks.iter().map(|c| c.len()).sum();
        self.st.lock().unwrap().live_bytes.insert(job, bytes);
        // Like the real thing: the page ends when its audio does.
        self.end(job, "finished");
        Ok(())
    }

    fn follow(&self, on: FeedFn) {
        on(Feed::Connected);
        *self.feed.lock().unwrap() = Some(Arc::new(on));
    }
}
