//! A fake mcast server for tests: jobs that run until told to end, one
//! sender per group, and events delivered synchronously.

use crate::backend::{Backend, BackendError, BackendResult, Entry, Feed, FeedFn, Job};
use serde_json::Value;
use std::collections::HashMap;
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

    fn media(&self) -> BackendResult<Vec<String>> {
        self.reachable()?;
        Ok(vec!["lunch.wav".into()])
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

    fn follow(&self, on: FeedFn) {
        on(Feed::Connected);
        *self.feed.lock().unwrap() = Some(Arc::new(on));
    }
}
