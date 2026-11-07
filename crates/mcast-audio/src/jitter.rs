//! A simple playout buffer that absorbs network jitter.
//!
//! The network side pushes decoded samples; the audio callback pulls them.
//! Playback starts once `target` frames are buffered. On underrun we output
//! silence and re-buffer; if the buffer grows past `max` (sender clock
//! faster than ours) the oldest audio is dropped back to `target`.

use std::collections::VecDeque;

pub struct JitterBuffer {
    samples: VecDeque<f32>,
    channels: usize,
    target_frames: usize,
    max_frames: usize,
    playing: bool,
    pub underruns: u64,
    pub overflows: u64,
}

impl JitterBuffer {
    pub fn new(channels: u16, target_frames: usize) -> Self {
        let target_frames = target_frames.max(1);
        Self {
            samples: VecDeque::with_capacity(target_frames * 4 * channels as usize),
            channels: channels as usize,
            target_frames,
            max_frames: target_frames * 3,
            playing: false,
            underruns: 0,
            overflows: 0,
        }
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn buffered_frames(&self) -> usize {
        self.samples.len() / self.channels
    }

    pub fn push(&mut self, samples: &[f32]) {
        self.samples.extend(samples);
        if self.buffered_frames() > self.max_frames {
            let excess = (self.buffered_frames() - self.target_frames) * self.channels;
            self.samples.drain(..excess);
            self.overflows += 1;
        }
    }

    pub fn push_silence(&mut self, frames: usize) {
        let n = frames * self.channels;
        self.samples.extend(std::iter::repeat(0.0).take(n));
    }

    /// Drop everything and wait to re-buffer (e.g. the stream restarted).
    pub fn reset(&mut self) {
        self.samples.clear();
        self.playing = false;
    }

    /// Fill `out` with interleaved samples, padding with silence if needed.
    pub fn pull(&mut self, out: &mut [f32]) {
        if !self.playing {
            if self.buffered_frames() < self.target_frames {
                out.fill(0.0);
                return;
            }
            self.playing = true;
        }
        let n = out.len().min(self.samples.len());
        for (dst, src) in out.iter_mut().zip(self.samples.drain(..n)) {
            *dst = src;
        }
        if n < out.len() {
            out[n..].fill(0.0);
            self.playing = false;
            self.underruns += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_for_target_then_plays() {
        let mut jb = JitterBuffer::new(1, 4);
        let mut out = [9.0; 2];
        jb.push(&[1.0, 2.0, 3.0]);
        jb.pull(&mut out);
        assert_eq!(out, [0.0, 0.0]); // still buffering
        jb.push(&[4.0]);
        jb.pull(&mut out);
        assert_eq!(out, [1.0, 2.0]);
    }

    #[test]
    fn underrun_pads_and_rebuffers() {
        let mut jb = JitterBuffer::new(1, 2);
        jb.push(&[1.0, 2.0]);
        let mut out = [9.0; 3];
        jb.pull(&mut out);
        assert_eq!(out, [1.0, 2.0, 0.0]);
        assert_eq!(jb.underruns, 1);
        jb.push(&[5.0]);
        jb.pull(&mut out);
        assert_eq!(out, [0.0; 3]); // re-buffering
    }

    #[test]
    fn overflow_trims_to_target() {
        let mut jb = JitterBuffer::new(2, 2); // max 6 frames
        jb.push(&[0.0; 14]); // 7 frames
        assert_eq!(jb.buffered_frames(), 2);
        assert_eq!(jb.overflows, 1);
    }
}
