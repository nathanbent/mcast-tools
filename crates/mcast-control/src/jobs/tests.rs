use super::*;
use crate::request::SourceRequest;
use std::net::SocketAddr;

fn tone(group: &str) -> SendRequest {
    SendRequest {
        group: group.parse().unwrap(),
        sap: false,
        ..SendRequest::default()
    }
}

fn jobs() -> (Jobs, Arc<Mutex<Vec<Event>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let e = events.clone();
    let jobs = Jobs::new(
        Interface::Any,
        Arc::new(move |ev| e.lock().unwrap().push(ev)),
    );
    (jobs, events)
}

#[test]
fn rejects_a_second_page_to_the_same_group() {
    let (jobs, _) = jobs();
    let first = jobs.start_sender(tone("239.255.77.11:5004")).unwrap();
    assert_eq!(first.state, JobState::Running);
    match jobs.start_sender(tone("239.255.77.11:5004")) {
        Err(StartError::Conflict { job, .. }) => assert_eq!(job, first.id),
        other => panic!("expected a conflict, got {other:?}"),
    }
    // Another group is fine.
    let other = jobs.start_sender(tone("239.255.77.12:5004")).unwrap();
    // Once the first stops, its group is free again.
    let stopped = jobs.stop(first.id, Duration::from_secs(5)).unwrap();
    assert_eq!(stopped.state, JobState::Stopped);
    let again = jobs.start_sender(tone("239.255.77.11:5004")).unwrap();
    jobs.stop(again.id, Duration::from_secs(5));
    jobs.stop(other.id, Duration::from_secs(5));
}

#[test]
fn stopping_cuts_a_chime_short() {
    let (jobs, _) = jobs();
    // A long lead-in, then a chime with nothing after it: all of it would
    // play out before a stop took effect if stops waited for chimes.
    let req = SendRequest {
        source: SourceRequest::Chime,
        chime: Some(crate::request::ChimeRequest::BuiltIn(
            mcast_audio::chime::BuiltIn::Westminster,
        )),
        lead_in_ms: 1000.0,
        ..tone("239.255.77.16:5004")
    };
    let job = jobs.start_sender(req).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let started = std::time::Instant::now();
    let stopped = jobs.stop(job.id, Duration::from_secs(5)).unwrap();
    assert_eq!(stopped.state, JobState::Stopped);
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "took {:?} to stop",
        started.elapsed()
    );
}

#[test]
fn bad_requests_fail_up_front_and_leave_no_job() {
    let (jobs, _) = jobs();
    let req = SendRequest {
        source: SourceRequest::File {
            path: "/nonexistent/page.wav".into(),
            looped: false,
        },
        ..tone("239.255.77.13:5004")
    };
    assert!(matches!(
        jobs.start_sender(req),
        Err(StartError::Invalid(_))
    ));
    assert!(jobs.list().is_empty());
}

#[test]
fn finished_jobs_report_stats_and_events() {
    let (jobs, events) = jobs();
    let req = SendRequest {
        duration_s: Some(0.2),
        tail_ms: 0.0,
        ..tone("239.255.77.14:5004")
    };
    let job = jobs.start_sender(req).unwrap();
    let done = jobs.stop_when_done(job.id);
    assert_eq!(done.state, JobState::Finished);
    let Some(JobStats::Send(s)) = done.stats else {
        panic!("no send stats");
    };
    assert!(s.packets > 0 && (s.seconds - 0.2).abs() < 0.05, "{s:?}");
    let events = events.lock().unwrap();
    assert!(matches!(events.first(), Some(Event::JobStarted { .. })));
    assert!(matches!(events.last(), Some(Event::JobEnded { .. })));
}

#[test]
fn receives_from_a_sender() {
    let (jobs, _) = jobs();
    let rx = jobs
        .start_receiver(
            RecvRequest {
                group: Some("239.255.77.15:5004".parse().unwrap()),
                sink: crate::request::SinkRequest::Wav {
                    path: std::env::temp_dir().join("mcast-control-test.wav"),
                },
                stats_interval_s: 0.1,
                ..RecvRequest::default()
            },
            &[],
        )
        .unwrap();
    let tx = jobs
        .start_sender(SendRequest {
            duration_s: Some(0.3),
            ..tone("239.255.77.15:5004")
        })
        .unwrap();
    jobs.stop_when_done(tx.id);
    let rx = jobs.stop(rx.id, Duration::from_secs(5)).unwrap();
    let Some(JobStats::Recv(s)) = rx.stats else {
        panic!("no receive stats");
    };
    assert!(s.packets > 100, "{s:?}");
}

impl Jobs {
    fn stop_when_done(&self, id: JobId) -> JobInfo {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let j = self.get(id).unwrap();
            if !j.state.is_active() || Instant::now() > deadline {
                return j;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_name_that_comes_late_relabels_the_lines() {
    use crate::request::TextRequest;
    let (jobs, events) = jobs();
    let group: SocketAddr = "239.255.77.17:5004".parse().unwrap();
    let listener = jobs
        .start_text(
            TextRequest {
                group: Some(group),
                send: false,
                ..TextRequest::default()
            },
            &[],
        )
        .unwrap();
    // A talker with no RTCP: its line arrives with only its address to go by.
    let talker = jobs
        .start_text(
            TextRequest {
                group: Some(group),
                rtcp: false,
                ..TextRequest::default()
            },
            &[],
        )
        .unwrap();
    jobs.send_text(talker.id, "hello\n").unwrap();
    wait_for("the line", || {
        !jobs.text_history(listener.id).unwrap().is_empty()
    });
    let heard = jobs.text_history(listener.id).unwrap();
    let ssrc = heard[0].ssrc.unwrap();
    assert_ne!(heard[0].from, "desk");

    // Its name, late: the line already heard takes it.
    let mut net = mcast_core::net::MulticastOptions::new(group).unwrap();
    net.loopback = true;
    let mut rtcp =
        mcast_core::rtcp::SenderRtcp::open(&net, ssrc, 1000, Some("desk"), Duration::from_secs(5))
            .unwrap();
    rtcp.report_now(Duration::ZERO);
    wait_for("the new name", || {
        jobs.text_history(listener.id).unwrap()[0].from == "desk"
    });
    assert!(events.lock().unwrap().iter().any(|e| matches!(
        e,
        Event::TextRenamed { job, ssrc: s, name } if *job == listener.id && *s == ssrc && name == "desk"
    )));
    drop(rtcp);
    jobs.stop(talker.id, Duration::from_secs(5));
    jobs.stop(listener.id, Duration::from_secs(5));
}

#[test]
fn text_channel_sends_and_hears_lines() {
    use crate::request::TextRequest;
    let (jobs, events) = jobs();
    let group: SocketAddr = "239.255.77.16:5004".parse().unwrap();
    let listener = jobs
        .start_text(
            TextRequest {
                group: Some(group),
                send: false,
                name: Some("ear".into()),
                ..TextRequest::default()
            },
            &[],
        )
        .unwrap();
    let talker = jobs
        .start_text(
            TextRequest {
                group: Some(group),
                name: Some("desk".into()),
                ..TextRequest::default()
            },
            &[],
        )
        .unwrap();
    assert_eq!(talker.kind, JobKind::Text);

    // A second sender on the channel from this server is refused, as is a page there.
    let again = TextRequest {
        group: Some(group),
        ..TextRequest::default()
    };
    assert!(matches!(
        jobs.start_text(again, &[]),
        Err(StartError::Conflict { .. })
    ));
    assert!(matches!(
        jobs.start_sender(tone("239.255.77.16:5004")),
        Err(StartError::Conflict { .. })
    ));

    let sent = jobs.send_text(talker.id, "hello\nsecond line").unwrap();
    assert_eq!(sent.len(), 2);
    assert!(sent.iter().all(|l| l.own && l.from == "desk"));
    wait_for("both lines to arrive", || {
        jobs.text_history(listener.id).unwrap().len() == 2
    });
    let heard = jobs.text_history(listener.id).unwrap();
    assert_eq!(heard[0].text, "hello");
    assert_eq!(heard[1].text, "second line");
    assert!(!heard[0].own);
    // The talker doesn't hear itself back, only its own record.
    assert_eq!(jobs.text_history(talker.id).unwrap().len(), 2);
    assert!(events
        .lock()
        .unwrap()
        .iter()
        .any(|e| matches!(e, Event::TextLine { line } if line.job == listener.id)));

    assert_eq!(
        jobs.send_text(listener.id, "hi"),
        Err(TextError::NotSending)
    );
    assert_eq!(jobs.send_text(talker.id, "  \n"), Err(TextError::Empty));
    assert_eq!(jobs.send_text(999, "hi"), Err(TextError::NoJob));
    let page = jobs.start_sender(tone("239.255.77.17:5004")).unwrap();
    assert_eq!(jobs.send_text(page.id, "hi"), Err(TextError::NotText));
    jobs.stop(page.id, Duration::from_secs(5));
    jobs.stop(talker.id, Duration::from_secs(5));
    assert_eq!(jobs.send_text(talker.id, "late"), Err(TextError::Ended));
    jobs.stop(listener.id, Duration::from_secs(5));
}

#[test]
fn files_go_from_sender_to_saved_copy() {
    use crate::request::{FileRecvRequest, FileSendRequest};
    let (jobs, events) = jobs();
    let dir = std::env::temp_dir().join(format!("mcast-jobs-files-{}", mcast_core::random_u32()));
    let (src, out) = (dir.join("src"), dir.join("out"));
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&out).unwrap();
    let data: Vec<u8> = (0..50_000u32).map(|i| (i * 7 % 251) as u8).collect();
    std::fs::write(src.join("site map.png"), &data).unwrap();
    // Something already called that, so the copy gets a new name.
    std::fs::write(out.join("site map.png"), b"older").unwrap();

    let group: SocketAddr = "239.255.77.18:5004".parse().unwrap();
    let rx = jobs
        .start_file_receiver(
            FileRecvRequest {
                group: Some(group),
                rtcp: false,
                out_dir: out.clone(),
                ..FileRecvRequest::default()
            },
            &[],
        )
        .unwrap();
    let tx = jobs
        .start_file_sender(FileSendRequest {
            group,
            files: vec![src.join("site map.png")],
            sap: false,
            rtcp: false,
            rate_kbps: 20_000,
            ..FileSendRequest::default()
        })
        .unwrap();
    // One sender per group.
    let again = FileSendRequest {
        group,
        files: vec![src.join("site map.png")],
        sap: false,
        ..FileSendRequest::default()
    };
    assert!(matches!(
        jobs.start_file_sender(again),
        Err(StartError::Conflict { .. })
    ));

    wait_for("the file to arrive", || {
        events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, Event::FileReceived { .. }))
    });
    let file = events
        .lock()
        .unwrap()
        .iter()
        .find_map(|e| match e {
            Event::FileReceived { job, file } if *job == rx.id => Some(file.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(file.name, "site map-2.png");
    assert_eq!(file.sent_as.as_deref(), Some("site map.png"));
    assert_eq!(file.verified, Some(true));
    assert_eq!(file.content_type.as_deref(), Some("image/png"));
    assert_eq!(std::fs::read(out.join("site map-2.png")).unwrap(), data);
    // No temporary files left behind.
    let names: Vec<String> = std::fs::read_dir(&out)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.iter().all(|n| !n.starts_with('.')), "{names:?}");

    wait_for("the sender to finish", || {
        jobs.get(tx.id).unwrap().state == JobState::Finished
    });
    let Some(JobStats::FileRecv(s)) = jobs.stop(rx.id, Duration::from_secs(5)).unwrap().stats
    else {
        panic!("no receive stats");
    };
    assert_eq!(s.received, 1);

    // Bad requests fail up front.
    let missing = FileSendRequest {
        files: vec![src.join("nope.png")],
        ..FileSendRequest::default()
    };
    assert!(matches!(
        jobs.start_file_sender(missing),
        Err(StartError::Invalid(_))
    ));
    let nowhere = FileRecvRequest {
        out_dir: dir.join("missing"),
        ..FileRecvRequest::default()
    };
    assert!(matches!(
        jobs.start_file_receiver(nowhere, &[]),
        Err(StartError::Invalid(_))
    ));
    std::fs::remove_dir_all(&dir).unwrap();
}
