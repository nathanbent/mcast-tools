//! Live audio pushed in from elsewhere (a browser's microphone, through
//! `mcast serve`), for a sender to play as it arrives.
//!
//! The pusher writes samples with [`LiveFeed::push`] and ends the page with
//! [`LiveFeed::close`]. The sender reads in real time: it waits for a little
//! audio before starting (and after running dry), sends silence while
//! nothing has arrived, drains what's left once the feed is closed, and
//! then ends. Audio pushed during the opening chime is kept and played
//! after it, so a speaker can start straight away.

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Audio buffered before playing starts, or starts again after a gap.
pub const PREBUFFER: Duration = Duration::from_millis(120);
/// Most audio held back; beyond this the oldest is dropped (the pusher's
/// clock is faster than ours, or it sent a burst).
pub const MAX_BACKLOG: Duration = Duration::from_secs(5);
/// Nothing pushed for this long, and not closed: the pusher has gone.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(20);

struct Inner {
    samples: VecDeque<f32>,
    closed: bool,
    last_push: Instant,
    /// Samples dropped because the backlog was full.
    dropped: u64,
    /// The first byte of a 16-bit sample split between pushes.
    odd_byte: Option<u8>,
}

/// A buffer of live audio shared between whoever pushes it and the sender.
#[derive(Clone)]
pub struct LiveFeed {
    rate: u32,
    channels: u16,
    inner: Arc<Mutex<Inner>>,
}

impl fmt::Debug for LiveFeed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LiveFeed({} Hz, {} ch)", self.rate, self.channels)
    }
}

impl LiveFeed {
    /// A feed of interleaved samples at `rate` with `channels`.
    pub fn new(rate: u32, channels: u16) -> Self {
        Self {
            rate,
            channels,
            inner: Arc::new(Mutex::new(Inner {
                samples: VecDeque::new(),
                closed: false,
                last_push: Instant::now(),
                dropped: 0,
                odd_byte: None,
            })),
        }
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn samples_for(&self, d: Duration) -> usize {
        (d.as_secs_f64() * self.rate as f64).round() as usize * self.channels as usize
    }

    /// Add interleaved samples (-1..1). Ignored once closed.
    pub fn push(&self, samples: &[f32]) {
        let max = self.samples_for(MAX_BACKLOG);
        let mut st = self.lock();
        if st.closed {
            return;
        }
        st.last_push = Instant::now();
        st.samples.extend(samples);
        let over = st.samples.len().saturating_sub(max);
        if over > 0 {
            // Whole frames only.
            let over = over.div_ceil(self.channels as usize) * self.channels as usize;
            let over = over.min(st.samples.len());
            st.samples.drain(..over);
            st.dropped += over as u64;
        }
    }

    /// Add 16-bit little-endian PCM bytes, however they were split: a
    /// sample cut between two pushes is put back together.
    pub fn push_s16le(&self, bytes: &[u8]) {
        let mut joined = Vec::with_capacity(bytes.len() + 1);
        joined.extend(self.lock().odd_byte.take());
        joined.extend_from_slice(bytes);
        let samples: Vec<f32> = joined
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
            .collect();
        if joined.len() % 2 == 1 {
            self.lock().odd_byte = joined.last().copied();
        }
        self.push(&samples);
    }

    /// The page is over: play what's left, then end.
    pub fn close(&self) {
        self.lock().closed = true;
    }

    pub fn is_closed(&self) -> bool {
        self.lock().closed
    }

    /// Samples dropped because too much was held back.
    pub fn dropped(&self) -> u64 {
        self.lock().dropped
    }
}

/// The sender's side: fills each chunk in real time.
pub(crate) struct LiveReader {
    feed: LiveFeed,
    /// Playing (rather than waiting for the prebuffer).
    flowing: bool,
}

impl LiveReader {
    pub(crate) fn new(feed: LiveFeed) -> Self {
        Self {
            feed,
            flowing: false,
        }
    }

    /// Fill `out` (interleaved) with what has arrived, padding gaps with
    /// silence. Returns how many samples are audio-or-silence to send:
    /// `out.len()`, or fewer once the feed is closed and drained (the end).
    pub(crate) fn fill(&mut self, out: &mut [f32]) -> usize {
        let prebuffer = self.feed.samples_for(PREBUFFER);
        let mut st = self.feed.lock();
        let gone = !st.closed && st.last_push.elapsed() > IDLE_TIMEOUT;
        if gone {
            st.closed = true;
        }
        if !self.flowing {
            // Once closed, whatever arrived plays without waiting for more.
            if st.samples.len() >= prebuffer || (st.closed && !st.samples.is_empty()) {
                self.flowing = true;
            } else if st.closed {
                return 0;
            } else {
                out.fill(0.0);
                return out.len();
            }
        }
        let n = out.len().min(st.samples.len());
        for (o, s) in out[..n].iter_mut().zip(st.samples.drain(..n)) {
            *o = s;
        }
        if n < out.len() {
            if st.closed {
                return n;
            }
            // Ran dry: silence, and wait for a prebuffer's worth again.
            out[n..].fill(0.0);
            self.flowing = false;
        }
        out.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_plays_pads_and_ends() {
        let feed = LiveFeed::new(1000, 1); // 1000 Hz: 1 sample per ms
        let mut r = LiveReader::new(feed.clone());
        let mut out = [9.0f32; 50];
        // Nothing yet: silence.
        assert_eq!(r.fill(&mut out), 50);
        assert!(out.iter().all(|s| *s == 0.0));
        // Less than the prebuffer (120 samples): still silence.
        feed.push(&[0.5; 100]);
        assert_eq!(r.fill(&mut out), 50);
        assert!(out.iter().all(|s| *s == 0.0));
        // Enough: it plays, in order.
        feed.push(&[0.25; 100]);
        assert_eq!(r.fill(&mut out), 50);
        assert!(out.iter().all(|s| *s == 0.5));
        r.fill(&mut out);
        for _ in 0..2 {
            assert_eq!(r.fill(&mut out), 50);
            assert!(out.iter().all(|s| *s == 0.25));
        }
        // Run dry: silence, until a prebuffer's worth arrives again.
        assert_eq!(r.fill(&mut out), 50);
        assert!(out.iter().all(|s| *s == 0.0));
        feed.push(&[0.5; 40]);
        assert_eq!(r.fill(&mut out), 50);
        assert!(out.iter().all(|s| *s == 0.0));
        // Closed with a little left: it plays without waiting, then ends.
        feed.close();
        feed.push(&[1.0; 30]); // after closing: ignored
        assert_eq!(r.fill(&mut out), 40);
        assert!(out[..40].iter().all(|s| *s == 0.5));
        assert_eq!(r.fill(&mut out), 0);
    }

    #[test]
    fn keeps_a_bounded_backlog_and_reads_pcm() {
        let feed = LiveFeed::new(1000, 2);
        // 6 s of stereo: the oldest second goes.
        feed.push(&vec![0.1; 12_000]);
        assert_eq!(feed.dropped(), 2_000);
        assert_eq!(feed.lock().samples.len(), 10_000);

        // 0.5, -0.5, then 0.25 split across two pushes.
        let feed = LiveFeed::new(1000, 1);
        feed.push_s16le(&[0x00, 0x40, 0x00, 0xc0, 0x00]);
        feed.push_s16le(&[0x20]);
        let got: Vec<f32> = feed.lock().samples.iter().copied().collect();
        assert_eq!(got, [0.5, -0.5, 0.25]);
    }
}
