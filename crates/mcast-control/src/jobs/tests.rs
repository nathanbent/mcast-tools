use super::*;
use crate::request::SourceRequest;

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
