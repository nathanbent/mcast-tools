//! The Probes tab's state: the probe list, the selected probe's reports
//! (for its timeline), and changes to a probe's configuration.

use crate::api::ApiError;
use crate::app::{App, Level, Msg, Tab};
use crate::model::{Probe, Report};
use serde_json::{json, Value};

/// Probe settings typed in a prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeField {
    Label,
    Groups,
    Interval,
}

impl ProbeField {
    pub fn prompt(self) -> &'static str {
        match self {
            ProbeField::Label => "Label (empty for none):",
            ProbeField::Groups => "Groups to watch besides announced ones, e.g. 239.192.2.1:10000 (space or comma between):",
            ProbeField::Interval => "Seconds per report and check-in (1-3600):",
        }
    }
}

impl App {
    /// A probe result from a background thread.
    pub(crate) fn probe_msg(&mut self, msg: Msg) {
        let fail = |app: &mut App, e: ApiError| app.say(e.message, Level::Error);
        match msg {
            Msg::Probes(r) => match r {
                Ok(mut p) => {
                    p.sort_by(|a, b| a.id.cmp(&b.id));
                    self.probes = p;
                    self.want_reports();
                }
                Err(e) => fail(self, e),
            },
            Msg::Reports(id, r) => {
                if self.reports_loading.as_deref() == Some(id.as_str()) {
                    self.reports_loading = None;
                }
                match r {
                    Ok(more) => {
                        let mut all = match self.reports.take() {
                            Some((have, old)) if have == id => old,
                            _ => Vec::new(),
                        };
                        let last = all.last().map(|r| r.end_ms);
                        all.extend(
                            more.into_iter()
                                .filter(|r| last.map_or(true, |l| r.end_ms > l)),
                        );
                        let excess = all.len().saturating_sub(360);
                        all.drain(..excess);
                        self.reports = Some((id, all));
                    }
                    Err(e) if e.status == 404 => {}
                    Err(e) => fail(self, e),
                }
            }
            Msg::ProbeChanged(r, what) => {
                self.busy = false;
                match r {
                    Ok(p) => {
                        self.say(format!("{}: {what}", p.id), Level::Ok);
                        self.put_probe(*p);
                    }
                    Err(e) => fail(self, e),
                }
            }
            _ => {}
        }
    }

    /// A probe event from the server; false if it isn't one.
    pub(crate) fn probe_event(&mut self, e: &Value) -> bool {
        match e["type"].as_str().unwrap_or("") {
            "probe_updated" => {
                if let Ok(p) = serde_json::from_value::<Probe>(e["probe"].clone()) {
                    let id = p.id.clone();
                    self.put_probe(p);
                    // Keep the selected probe's timeline current.
                    if self.selected_probe().is_some_and(|p| p.id == id) {
                        self.load_reports(id);
                    }
                }
            }
            "probe_removed" => {
                if let Some(id) = e["id"].as_str() {
                    self.probes.retain(|p| p.id != id);
                    self.want_reports();
                }
            }
            _ => return false,
        }
        true
    }

    fn put_probe(&mut self, p: Probe) {
        match self.probes.binary_search_by(|q| q.id.cmp(&p.id)) {
            Ok(i) => self.probes[i] = p,
            Err(i) => self.probes.insert(i, p),
        }
        self.want_reports();
    }

    pub fn selected_probe(&self) -> Option<&Probe> {
        self.probes
            .get(self.selected[Tab::Probes.index()].min(self.probes.len().saturating_sub(1)))
    }

    /// The reports shown belong to the selected probe.
    pub fn probe_reports(&self) -> Option<&[Report]> {
        let p = self.selected_probe()?;
        match &self.reports {
            Some((id, r)) if *id == p.id => Some(r),
            _ => None,
        }
    }

    /// Fetch the selected probe's reports if they aren't here yet.
    pub fn want_reports(&mut self) {
        let Some(id) = self.selected_probe().map(|p| p.id.clone()) else {
            return;
        };
        if self.reports.as_ref().is_some_and(|(have, _)| *have == id) {
            return;
        }
        self.load_reports(id);
    }

    /// Fetch reports newer than those held (all of them for another probe).
    fn load_reports(&mut self, id: String) {
        if self.reports_loading.as_deref() == Some(id.as_str()) {
            return;
        }
        let after = match &self.reports {
            Some((have, r)) if *have == id => r.last().map(|r| r.end_ms),
            _ => None,
        };
        self.reports_loading = Some(id.clone());
        let path = format!(
            "/probes/{}/reports{}",
            crate::api::encode(&id),
            after.map_or(String::new(), |a| format!("?after_ms={a}"))
        );
        self.spawn(move |c| Msg::Reports(id, c.get(&path)));
    }

    /// Change the selected probe's configuration with `f`, and save it.
    pub fn configure_probe(
        &mut self,
        what: &str,
        f: impl FnOnce(&mut Value) -> Result<(), String>,
    ) {
        let Some(p) = self.selected_probe() else {
            return;
        };
        let (id, mut config) = (p.id.clone(), p.config.clone());
        if let Err(e) = f(&mut config) {
            return self.say(e, Level::Error);
        }
        let what = what.to_string();
        self.busy = true;
        self.spawn(move |c| {
            let path = format!("/probes/{}/config", crate::api::encode(&id));
            let r = c.put(&path, &config).and_then(|v| {
                serde_json::from_value(v).map_err(|e| ApiError::from(std::io::Error::other(e)))
            });
            Msg::ProbeChanged(r, what)
        });
    }

    /// Apply a typed setting.
    pub fn set_probe_field(&mut self, field: ProbeField, text: String) {
        let text = text.trim().to_string();
        match field {
            ProbeField::Label => self.configure_probe("label saved", |c| {
                c["label"] = if text.is_empty() {
                    Value::Null
                } else {
                    text.into()
                };
                Ok(())
            }),
            ProbeField::Groups => self.configure_probe("groups saved", |c| {
                let groups: Vec<&str> = text.split([' ', ',']).filter(|g| !g.is_empty()).collect();
                if let Some(bad) = groups.iter().find(|g| !crate::model::is_multicast(g)) {
                    return Err(format!("{bad} isn't a multicast group and port"));
                }
                c["groups"] = json!(groups);
                Ok(())
            }),
            ProbeField::Interval => self.configure_probe("report interval saved", |c| {
                let s: f64 = text
                    .parse()
                    .map_err(|_| "that isn't a number of seconds".to_string())?;
                c["report_interval_s"] = json!(s);
                Ok(())
            }),
        }
    }

    pub fn toggle_probe(&mut self, key: &'static str, on: &'static str, off: &'static str) {
        let now = self.selected_probe().is_some_and(|p| p.flag(key));
        self.configure_probe(if now { off } else { on }, |c| {
            c[key] = json!(!now);
            Ok(())
        });
    }

    pub fn reset_probe(&mut self) {
        let Some(id) = self.selected_probe().map(|p| p.id.clone()) else {
            return;
        };
        self.busy = true;
        self.spawn(move |c| {
            let path = format!("/probes/{}/config", crate::api::encode(&id));
            let r = c.request_delete_json(&path).and_then(|v| {
                serde_json::from_value(v).map_err(|e| ApiError::from(std::io::Error::other(e)))
            });
            Msg::ProbeChanged(r, "back to the defaults".into())
        });
    }

    pub fn forget_probe(&mut self, id: String) {
        self.spawn(move |c| {
            let path = format!("/probes/{}", crate::api::encode(&id));
            Msg::Done(
                c.delete(&path).map(|_| format!("Forgot {id}")),
                Some("probes"),
            )
        });
    }
}
