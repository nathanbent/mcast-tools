//! Human-readable descriptions shared by the CLI and job listings.

use crate::request::SendRequest;
use mcast_audio::chime::ChimeSource;
use mcast_audio::process::Mode;
use mcast_audio::sender::Sender;

/// e.g. "PCMU/8000/1 sending to 239.192.2.1:10000 (pt 0, 20 ms packets, voice processing)".
pub fn send_summary(s: &Sender, req: &SendRequest) -> String {
    let spec = s.spec();
    let (src_rate, src_ch) = s.source_format();
    let conversion = if (src_rate, src_ch) != (spec.sample_rate, spec.channels) {
        format!(", converted from {src_rate} Hz/{src_ch}ch")
    } else {
        String::new()
    };
    let destination = match &req.render {
        Some(path) => format!("rendering to {}", path.display()),
        None => format!("sending to {}", req.group),
    };
    let mut details = format!(
        "pt {}, {} ms packets{conversion}",
        s.payload_type(),
        s.ptime().as_secs_f64() * 1000.0
    );
    let mode = req.mode();
    if mode != Mode::None {
        details += &format!(", {mode} processing");
    }
    if let Some(v) = req.volume_db {
        details += &format!(", {v:+} dB");
    }
    if let Some(c) = req.chime() {
        details += &match &c.source {
            ChimeSource::BuiltIn => ", built-in chime".to_string(),
            ChimeSource::File(p) => format!(", chime {}", p.display()),
        };
    }
    format!("{spec} {destination} ({details})")
}

/// What loudness normalisation did, for file sources with processing.
pub fn loudness_note(s: &Sender) -> Option<String> {
    let l = s.loudness()?;
    Some(match l.measured {
        Some(m) => format!(
            "loudness: measured {m:.1} LUFS after processing; {:+.1} dB to reach {} LUFS",
            l.gain_db, l.target
        ),
        None => "loudness: source is silent; no normalisation".into(),
    })
}
