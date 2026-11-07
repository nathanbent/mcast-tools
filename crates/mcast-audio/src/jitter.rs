//! A lock-free playout buffer that absorbs network jitter.
//!
//! The network thread pushes decoded samples through a [`JitterWriter`]; the
//! audio callback pulls them through a [`JitterReader`]. They share a
//! single-producer single-consumer ring buffer and a few atomics, so the
//! real-time audio thread never waits on a lock or allocates.
//!
//! Playback starts once `target` frames are buffered. On underrun the reader
//! outputs silence and re-buffers; if the buffer grows past `max` (sender
//! clock faster than ours) the reader drops the oldest audio back to `target`.

use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// State shared by the two halves.
#[derive(Default)]
struct Shared {
    underruns: AtomicU64,
    overflows: AtomicU64,
    /// Samples the writer couldn't fit (the reader had stopped pulling).
    dropped: AtomicU64,
    /// Samples written before the last [`JitterWriter::reset`]; the reader
    /// discards everything up to here.
    discard_until: AtomicU64,
}

/// Network-side half: pushes decoded, interleaved samples.
pub struct JitterWriter {
    producer: Producer<f32>,
    shared: Arc<Shared>,
    channels: usize,
    capacity: usize,
    written: u64,
}

/// Audio-callback half: pulls interleaved samples for playback.
pub struct JitterReader {
    consumer: Consumer<f32>,
    shared: Arc<Shared>,
    channels: usize,
    target: usize,
    max: usize,
    playing: bool,
    read: u64,
}

/// Create a buffer for `channels`-channel audio that plays once
/// `target_frames` are queued.
pub fn jitter_buffer(channels: u16, target_frames: usize) -> (JitterWriter, JitterReader) {
    let channels = channels.max(1) as usize;
    let target = target_frames.max(1);
    let max = target * 3;
    // Room above `max` so a burst of packets isn't refused before the reader
    // gets a chance to trim.
    let capacity = (max + target) * channels;
    let (producer, consumer) = RingBuffer::new(capacity);
    let shared = Arc::new(Shared::default());
    (
        JitterWriter {
            producer,
            shared: shared.clone(),
            channels,
            capacity,
            written: 0,
        },
        JitterReader {
            consumer,
            shared,
            channels,
            target,
            max,
            playing: false,
            read: 0,
        },
    )
}

/// Counters for reporting, read from the network side.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct JitterStats {
    pub underruns: u64,
    pub overflows: u64,
    pub dropped: u64,
    pub buffered_frames: usize,
}

impl JitterWriter {
    /// Queue interleaved samples. Whole frames only are queued; if the
    /// buffer is full (the reader stopped pulling), the excess is dropped.
    pub fn push(&mut self, samples: &[f32]) {
        let room = self.producer.slots() / self.channels * self.channels;
        let take = samples.len().min(room);
        if take < samples.len() {
            self.shared
                .dropped
                .fetch_add((samples.len() - take) as u64, Ordering::Relaxed);
        }
        if take > 0 && self.producer.push_entire_slice(&samples[..take]).is_ok() {
            self.written += take as u64;
        }
    }

    /// Drop everything queued so far and re-buffer (e.g. the stream restarted).
    pub fn reset(&mut self) {
        self.shared
            .discard_until
            .store(self.written, Ordering::Release);
    }

    pub fn stats(&self) -> JitterStats {
        JitterStats {
            underruns: self.shared.underruns.load(Ordering::Relaxed),
            overflows: self.shared.overflows.load(Ordering::Relaxed),
            dropped: self.shared.dropped.load(Ordering::Relaxed),
            buffered_frames: (self.capacity - self.producer.slots()) / self.channels,
        }
    }
}

impl JitterReader {
    pub fn channels(&self) -> usize {
        self.channels
    }

    fn skip(&mut self, samples: usize) {
        if let Ok(chunk) = self.consumer.read_chunk(samples) {
            chunk.commit_all();
            self.read += samples as u64;
        }
    }

    /// Fill `out` with interleaved samples, padding with silence if needed.
    /// Real-time safe: no locks, no allocation.
    pub fn pull(&mut self, out: &mut [f32]) {
        let discard = self.shared.discard_until.load(Ordering::Acquire);
        if discard > self.read {
            let n = ((discard - self.read) as usize).min(self.consumer.slots());
            self.skip(n);
            self.playing = false;
        }

        let available = self.consumer.slots();
        if available > self.max * self.channels {
            self.skip(available - self.target * self.channels);
            self.shared.overflows.fetch_add(1, Ordering::Relaxed);
        }

        if !self.playing {
            if self.consumer.slots() < self.target * self.channels {
                out.fill(0.0);
                return;
            }
            self.playing = true;
        }
        let (filled, rest) = self.consumer.pop_partial_slice(out);
        self.read += filled.len() as u64;
        if !rest.is_empty() {
            rest.fill(0.0);
            self.playing = false;
            self.shared.underruns.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_for_target_then_plays() {
        let (mut w, mut r) = jitter_buffer(1, 4);
        let mut out = [9.0; 2];
        w.push(&[1.0, 2.0, 3.0]);
        r.pull(&mut out);
        assert_eq!(out, [0.0, 0.0]); // still buffering
        w.push(&[4.0]);
        r.pull(&mut out);
        assert_eq!(out, [1.0, 2.0]);
    }

    #[test]
    fn underrun_pads_and_rebuffers() {
        let (mut w, mut r) = jitter_buffer(1, 2);
        w.push(&[1.0, 2.0]);
        let mut out = [9.0; 3];
        r.pull(&mut out);
        assert_eq!(out, [1.0, 2.0, 0.0]);
        assert_eq!(w.stats().underruns, 1);
        w.push(&[5.0]);
        r.pull(&mut out);
        assert_eq!(out, [0.0; 3]); // re-buffering
    }

    #[test]
    fn overflow_trims_to_target() {
        let (mut w, mut r) = jitter_buffer(2, 2); // max 6 frames
        w.push(&[0.0; 14]); // 7 frames
        let mut out = [9.0; 2];
        r.pull(&mut out);
        let s = w.stats();
        assert_eq!(s.overflows, 1);
        assert_eq!(s.buffered_frames, 1); // trimmed to 2, then played 1
    }

    #[test]
    fn reset_discards_only_older_audio() {
        let (mut w, mut r) = jitter_buffer(1, 2);
        w.push(&[1.0, 2.0, 3.0]);
        w.reset();
        w.push(&[7.0, 8.0]);
        let mut out = [9.0; 2];
        r.pull(&mut out);
        assert_eq!(out, [7.0, 8.0]);
    }

    #[test]
    fn full_buffer_drops_whole_frames() {
        let (mut w, _r) = jitter_buffer(2, 1); // capacity 4 frames
        w.push(&[1.0; 11]); // 5.5 frames
        let s = w.stats();
        assert_eq!(s.buffered_frames, 4);
        assert_eq!(s.dropped, 3);
    }

    #[test]
    fn threads_stream_without_loss() {
        let (mut w, mut r) = jitter_buffer(1, 64);
        let reader = std::thread::spawn(move || {
            let mut got = Vec::new();
            let mut out = [0.0f32; 16];
            while got.len() < 10_000 {
                r.pull(&mut out);
                got.extend(out.iter().copied().filter(|&x| x != 0.0));
                std::thread::yield_now();
            }
            got
        });
        let mut next = 1.0f32;
        while next <= 10_000.0 {
            if w.stats().buffered_frames < 128 {
                let chunk: Vec<f32> = (0..8).map(|i| next + i as f32).collect();
                w.push(&chunk);
                next += 8.0;
            } else {
                std::thread::yield_now();
            }
        }
        // Trailing silence so the last samples reach the playout target.
        w.push(&[0.0; 64]);
        let got = reader.join().unwrap();
        // Samples arrive in order; none duplicated.
        assert!(got.windows(2).all(|p| p[1] > p[0]), "out of order");
    }
}
