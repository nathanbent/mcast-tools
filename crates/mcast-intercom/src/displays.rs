//! Text pages on displays: what the panel's display page shows (the
//! board), and real-time text to zones' display groups through the mcast
//! server's text channels (`mcast text recv` shows it).
//!
//! The runner says when a run reaches a text page ([`Change::Shown`]);
//! this keeps the page up while the run plays and for [`HOLD`] after.
//!
//! Display groups get each page as a page: its lines with its name and
//! priority, and its end when the run ends (`docs/text-page-metadata.md`),
//! so a display that only hears multicast shows it as the display page
//! does. Groups of a zone with `plain_displays`, and an mcast server too
//! old to send pages, get the words alone.

use crate::backend::Backend;
use crate::model::Priority;
use crate::now_ms;
use crate::runner::{Change, RunId, Shown};
use serde::Serialize;
use serde_json::json;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a text page stays up after its run ends.
pub const HOLD: Duration = Duration::from_secs(10);
/// How long an unused text channel stays open (it holds its group).
const IDLE: Duration = Duration::from_secs(300);
/// Pages kept on the board.
const KEEP: usize = 50;
/// The name the panel's text channels go by.
pub const CHANNEL_NAME: &str = "Intercom";

/// A text page on the board.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Page {
    pub run: RunId,
    /// Announcement id and name.
    pub announcement: String,
    pub name: String,
    /// The announcement's priority (`normal`, `high`, `emergency`).
    pub priority: Priority,
    /// The zones (ids) it's shown in.
    pub zones: Vec<String>,
    /// Its lines.
    pub text: Vec<String>,
    pub at_ms: u64,
    /// When it comes down: unset while its run plays.
    pub until_ms: Option<u64>,
}

/// Text pages, and the channels that carry them to display groups.
pub struct Displays {
    board: Mutex<Vec<Page>>,
    on_page: Box<dyn Fn(&Page) + Send + Sync>,
    work: Mutex<mpsc::Sender<Work>>,
    /// The pages each run sent to display groups as pages, to clear when
    /// it ends.
    sent: Mutex<HashMap<RunId, Vec<SentPage>>>,
}

/// A page sent as a page: its id, and the groups it went to.
type SentPage = (String, Vec<String>);

/// Something for the channels' thread to send.
enum Work {
    /// A page's lines to `groups`; with `page`, as a page.
    Text {
        groups: Vec<String>,
        text: Vec<String>,
        page: Option<serde_json::Value>,
    },
    /// The page comes down.
    Clear {
        groups: Vec<String>,
        id: String,
        now: bool,
    },
}

/// A page's id on the wire: its run and when it went up, as the API's
/// pages have them, so a display that gets it both ways can tell.
pub fn page_id(run: RunId, at_ms: u64) -> String {
    format!("{run}.{at_ms}")
}

impl Displays {
    /// `on_page` hears about each page put up or given an end.
    pub fn start(
        backend: Arc<dyn Backend>,
        on_page: impl Fn(&Page) + Send + Sync + 'static,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("intercom-displays".into())
            .spawn(move || Channels::new(backend).run(rx))
            .expect("starting the displays thread");
        Self {
            board: Mutex::new(Vec::new()),
            on_page: Box::new(on_page),
            work: Mutex::new(tx),
            sent: Mutex::default(),
        }
    }

    /// Hear about a change from the runner; never blocks on the network.
    pub fn observe(&self, c: &Change<'_>) {
        match c {
            Change::Shown(s) => self.show(s),
            Change::Run(run) if run.state.is_over() && !run.shown.is_empty() => {
                let until = if run.state == crate::RunState::Stopped {
                    now_ms()
                } else {
                    run.ended_ms.unwrap_or_else(now_ms) + HOLD.as_millis() as u64
                };
                let ended: Vec<Page> = {
                    let mut board = self.board.lock().unwrap();
                    board
                        .iter_mut()
                        .filter(|p| p.run == run.id && p.until_ms.is_none())
                        .map(|p| {
                            p.until_ms = Some(until);
                            p.clone()
                        })
                        .collect()
                };
                for p in &ended {
                    (self.on_page)(p);
                }
                let now = run.state == crate::RunState::Stopped;
                for (id, groups) in self
                    .sent
                    .lock()
                    .unwrap()
                    .remove(&run.id)
                    .unwrap_or_default()
                {
                    let _ = self
                        .work
                        .lock()
                        .unwrap()
                        .send(Work::Clear { groups, id, now });
                }
            }
            _ => {}
        }
    }

    fn show(&self, s: &Shown) {
        let page = Page {
            run: s.run,
            announcement: s.announcement.clone(),
            name: s.name.clone(),
            priority: s.priority,
            zones: s.zones.clone(),
            text: s.text.clone(),
            at_ms: s.at_ms,
            until_ms: None,
        };
        {
            let mut board = self.board.lock().unwrap();
            let now = now_ms();
            board.retain(|p| p.until_ms.is_none_or(|u| u > now));
            board.push(page.clone());
            let over = board.len().saturating_sub(KEEP);
            board.drain(..over);
        }
        (self.on_page)(&page);
        let (plain, paged): (Vec<String>, Vec<String>) = s
            .groups
            .iter()
            .cloned()
            .partition(|g| s.plain_groups.contains(g));
        let work = self.work.lock().unwrap();
        if !plain.is_empty() {
            let _ = work.send(Work::Text {
                groups: plain,
                text: s.text.clone(),
                page: None,
            });
        }
        if !paged.is_empty() {
            let id = page_id(s.run, s.at_ms);
            let mut sent = self.sent.lock().unwrap();
            // Runs that never ended here (the panel was told of no end) don't pile up.
            if sent.len() > KEEP {
                sent.clear();
            }
            sent.entry(s.run)
                .or_default()
                .push((id.clone(), paged.clone()));
            let _ = work.send(Work::Text {
                groups: paged,
                text: s.text.clone(),
                page: Some(json!({
                    "id": id,
                    "name": s.name,
                    "priority": s.priority,
                    "hold_s": HOLD.as_secs(),
                })),
            });
        }
    }

    /// The pages up now, newest first; with `zones`, those shown in any of them.
    pub fn pages(&self, zones: Option<&[String]>) -> Vec<Page> {
        let now = now_ms();
        let board = self.board.lock().unwrap();
        board
            .iter()
            .rev()
            .filter(|p| p.until_ms.is_none_or(|u| u > now))
            .filter(|p| zones.is_none_or(|z| p.zones.iter().any(|pz| z.contains(pz))))
            .cloned()
            .collect()
    }
}

/// The text channels the panel holds open, one per display group.
struct Channels {
    backend: Arc<dyn Backend>,
    /// Address → (job, last used).
    open: HashMap<String, (u64, Instant)>,
    /// The mcast server is too old to send pages: it gets the words.
    no_pages: bool,
}

impl Channels {
    fn new(backend: Arc<dyn Backend>) -> Self {
        Self {
            backend,
            open: HashMap::new(),
            no_pages: false,
        }
    }

    fn run(mut self, rx: mpsc::Receiver<Work>) {
        loop {
            match rx.recv_timeout(Duration::from_secs(30)) {
                Ok(Work::Text { groups, text, page }) => self.send(&groups, &text, page.as_ref()),
                Ok(Work::Clear { groups, id, now }) => self.clear(&groups, &id, now),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            self.close_idle(IDLE);
        }
        self.close_idle(Duration::ZERO);
    }

    /// The groups' addresses: as written, or by their phonebook names.
    fn addresses(&self, groups: &[String]) -> Vec<(String, String)> {
        let mut book = None;
        let mut out = Vec::new();
        for g in groups {
            let address = match g.trim().parse::<SocketAddr>() {
                Ok(a) => a.to_string(),
                Err(_) => {
                    let entries = book.get_or_insert_with(|| self.backend.phonebook());
                    match entries
                        .as_ref()
                        .ok()
                        .and_then(|b| b.iter().find(|e| e.name.eq_ignore_ascii_case(g.trim())))
                    {
                        Some(e) => e.group.clone(),
                        None => {
                            eprintln!("mcast intercom: display \"{g}\" isn't in the phonebook");
                            continue;
                        }
                    }
                }
            };
            out.push((g.clone(), address));
        }
        out
    }

    fn send(&mut self, groups: &[String], text: &[String], page: Option<&serde_json::Value>) {
        let line = text.join("\n");
        for (g, address) in self.addresses(groups) {
            if let Err(e) = self.send_to(&address, &line, page) {
                eprintln!("mcast intercom: showing text on {g}: {e}");
            }
        }
    }

    /// Take a page down on the groups whose channel is still open (a
    /// channel that's gone took its pages' displays' attention with it:
    /// they bring a page down by themselves when no end comes).
    fn clear(&mut self, groups: &[String], id: &str, now: bool) {
        if self.no_pages {
            return;
        }
        for (g, address) in self.addresses(groups) {
            let Some(&(job, _)) = self.open.get(&address) else {
                continue;
            };
            match self.backend.clear_page(job, id, now) {
                Ok(()) => {
                    self.open.insert(address, (job, Instant::now()));
                }
                Err(e) if e.status == 404 => self.no_pages = true,
                Err(e) => eprintln!("mcast intercom: clearing text on {g}: {e}"),
            }
        }
    }

    /// Send on the group's channel, opening it if need be, and once more
    /// on a new one if the old one has gone (the server restarted, say).
    fn send_to(
        &mut self,
        address: &str,
        line: &str,
        page: Option<&serde_json::Value>,
    ) -> Result<(), crate::BackendError> {
        for fresh in [false, true] {
            let job = match self.open.get(address) {
                Some((job, _)) if !fresh => *job,
                _ => {
                    if let Some((old, _)) = self.open.remove(address) {
                        let _ = self.backend.stop_job(old);
                    }
                    let job = self
                        .backend
                        .start_text(&json!({
                            "group": address,
                            "name": CHANNEL_NAME,
                            "sap": true,
                        }))?
                        .id;
                    self.open.insert(address.to_string(), (job, Instant::now()));
                    job
                }
            };
            let page = page.filter(|_| !self.no_pages);
            let mut sent = self.backend.send_text(job, line, page);
            // A server from before pages refuses the field: the words still go.
            if page.is_some() && sent.as_ref().is_err_and(|e| matches!(e.status, 400 | 422)) {
                self.no_pages = true;
                sent = self.backend.send_text(job, line, None);
            }
            match sent {
                Ok(()) => {
                    self.open.insert(address.to_string(), (job, Instant::now()));
                    return Ok(());
                }
                Err(e) if fresh || e.status == 0 => return Err(e),
                Err(_) => {}
            }
        }
        unreachable!("the second try returns")
    }

    fn close_idle(&mut self, idle: Duration) {
        let stale: Vec<String> = self
            .open
            .iter()
            .filter(|(_, (_, used))| used.elapsed() >= idle)
            .map(|(a, _)| a.clone())
            .collect();
        for a in stale {
            if let Some((job, _)) = self.open.remove(&a) {
                let _ = self.backend.stop_job(job);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::Fake;

    fn shown(run: RunId, zones: &[&str], groups: &[&str], text: &str) -> Shown {
        Shown {
            run,
            announcement: "closing".into(),
            name: "Closing".into(),
            priority: Priority::Normal,
            zones: zones.iter().map(|z| z.to_string()).collect(),
            groups: groups.iter().map(|g| g.to_string()).collect(),
            plain_groups: Vec::new(),
            text: vec![text.to_string()],
            at_ms: now_ms(),
        }
    }

    fn wait_for(fake: &Fake, n: usize) -> Vec<(String, String)> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let t = fake.texts();
            if t.len() >= n || Instant::now() > deadline {
                return t;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn puts_pages_up_and_sends_them_to_display_groups() {
        let fake = Arc::new(Fake::default());
        let heard: Arc<Mutex<Vec<Page>>> = Arc::default();
        let told = heard.clone();
        let d = Displays::start(fake.clone(), move |p| told.lock().unwrap().push(p.clone()));

        d.observe(&Change::Shown(&shown(
            1,
            &["hall"],
            &["239.1.2.3:5004", "Zone-1"],
            "Library closes soon",
        )));
        d.observe(&Change::Shown(&shown(2, &["gym"], &[], "Gym closed")));
        assert_eq!(d.pages(None).len(), 2);
        let hall = d.pages(Some(&["hall".to_string()]));
        assert_eq!(hall.len(), 1);
        assert_eq!(hall[0].text, ["Library closes soon"]);
        assert_eq!(hall[0].until_ms, None);

        // By address, and by phonebook name.
        let mut texts = wait_for(&fake, 2);
        texts.sort();
        assert_eq!(
            texts,
            [
                (
                    "239.1.1.1:5000".to_string(),
                    "Library closes soon".to_string()
                ),
                (
                    "239.1.2.3:5004".to_string(),
                    "Library closes soon".to_string()
                ),
            ]
        );
        // The channel stays open for the next page.
        d.observe(&Change::Shown(&shown(
            1,
            &["hall"],
            &["239.1.2.3:5004"],
            "Second page",
        )));
        assert_eq!(wait_for(&fake, 3).len(), 3);
        assert!(fake.running_on("239.1.2.3:5004").is_some());

        // When run 1 ends, its pages get an end; stopped, they come down now.
        let mut run = crate::Run::for_tests(1);
        run.state = crate::RunState::Done;
        run.ended_ms = Some(now_ms());
        run.shown.push(crate::runner::ShownText {
            part: None,
            round: 1,
            item: Some(0),
            text: vec!["x".into()],
            at_ms: 0,
        });
        d.observe(&Change::Run(&run));
        let hall = d.pages(Some(&["hall".to_string()]));
        assert_eq!(hall.len(), 2);
        assert!(hall.iter().all(|p| p.until_ms.is_some()));
        let mut run2 = run.clone();
        run2.id = 2;
        run2.state = crate::RunState::Stopped;
        d.observe(&Change::Run(&run2));
        std::thread::sleep(Duration::from_millis(5));
        assert!(d.pages(Some(&["gym".to_string()])).is_empty());
        // Everyone heard: 3 pages up, 3 given an end.
        assert_eq!(heard.lock().unwrap().len(), 6);
    }

    #[test]
    fn reopens_a_channel_that_went_away() {
        let fake = Arc::new(Fake::default());
        let mut ch = Channels::new(fake.clone());
        ch.send(&["239.9.9.9:5004".into()], &["one".into()], None);
        let job = fake.running_on("239.9.9.9:5004").unwrap();
        fake.end(job, "stopped");
        ch.send(&["239.9.9.9:5004".into()], &["two".into()], None);
        assert_eq!(fake.texts().len(), 2);
        assert_ne!(fake.running_on("239.9.9.9:5004"), Some(job));
        // Idle channels are closed, freeing their groups.
        ch.close_idle(Duration::ZERO);
        assert_eq!(fake.running_on("239.9.9.9:5004"), None);
        // A name that isn't in the phonebook is skipped.
        ch.send(&["Nowhere".into()], &["three".into()], None);
        assert_eq!(fake.texts().len(), 2);
    }

    #[test]
    fn display_groups_get_pages_as_pages_and_their_end() {
        let fake = Arc::new(Fake::default());
        let d = Displays::start(fake.clone(), |_| {});
        let mut s = shown(
            7,
            &["hall"],
            &["239.1.2.3:5004", "239.1.2.4:5004"],
            "Fire drill",
        );
        s.priority = Priority::Emergency;
        s.plain_groups = vec!["239.1.2.4:5004".into()];
        let id = page_id(7, s.at_ms);
        d.observe(&Change::Shown(&s));
        assert_eq!(wait_for(&fake, 2).len(), 2);
        // The page's name, priority and hold go to the group that takes
        // pages; the plain one gets only the words.
        let pages = fake.pages();
        assert_eq!(pages.len(), 1, "{pages:?}");
        assert_eq!(pages[0].0, "239.1.2.3:5004");
        assert_eq!(
            pages[0].1,
            json!({"id": id, "name": "Closing", "priority": "emergency", "hold_s": 10})
        );
        // Stopped by hand: it comes down at once, there and only there.
        let mut run = crate::Run::for_tests(7);
        run.state = crate::RunState::Stopped;
        run.shown.push(crate::runner::ShownText {
            part: None,
            round: 1,
            item: Some(0),
            text: vec!["x".into()],
            at_ms: 0,
        });
        d.observe(&Change::Run(&run));
        let deadline = Instant::now() + Duration::from_secs(5);
        while fake.cleared().is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(fake.cleared(), [("239.1.2.3:5004".to_string(), id, true)]);
    }

    #[test]
    fn an_older_server_gets_the_words() {
        let fake = Arc::new(Fake::default());
        fake.set_no_pages();
        let mut ch = Channels::new(fake.clone());
        let page = json!({"id": "1.2", "name": "X", "priority": "normal", "hold_s": 10});
        ch.send(&["239.9.9.8:5004".into()], &["one".into()], Some(&page));
        ch.send(&["239.9.9.8:5004".into()], &["two".into()], Some(&page));
        assert_eq!(fake.texts().len(), 2);
        assert!(fake.pages().is_empty());
        ch.clear(&["239.9.9.8:5004".into()], "1.2", false);
        assert!(fake.cleared().is_empty());
    }
}
