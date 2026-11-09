//! Puts RTP packets back in sequence order before they are decoded.
//!
//! Packets are held in slots indexed by sequence number and released in
//! order. When one is missing, later packets are held for up to `wait` in
//! case it is only late (reordered by the network); after that it is
//! declared lost and the decoder conceals it. Decoding has to happen in
//! order: G.722 and Opus carry state from one packet to the next, and Opus
//! recovers a lost packet from the forward error correction in the next one.
//!
//! Waiting costs nothing audible as long as `wait` is less than the playout
//! latency: the jitter buffer keeps playing what it already holds.

use std::time::{Duration, Instant};

/// Packets that can be held at once. A power of two that divides 65536, so
/// `seq % SLOTS` stays consistent across sequence-number wraparound.
const SLOTS: usize = 128;

/// Jumps larger than this (either way) are a restarted stream, not loss.
const MAX_JUMP: i32 = 3000;

/// What [`Reorder`] hands back, in playout order.
#[derive(Debug, PartialEq, Eq)]
pub enum Released<'a> {
    /// The next packet's payload.
    Packet(&'a [u8]),
    /// `count` packets never arrived; `next` is the payload that follows
    /// them (for codecs that recover a loss from the next packet).
    Lost { count: u16, next: &'a [u8] },
    /// The stream jumped (sender restarted, or a long outage): drop any
    /// buffered audio and start over.
    Reset,
}

#[derive(Default)]
struct Slot {
    seq: u16,
    full: bool,
    payload: Vec<u8>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReorderStats {
    /// Packets declared lost (never arrived, or arrived too late).
    pub lost: u64,
    /// Packets that arrived after their slot had been played or written off.
    pub late: u64,
    /// Packets that arrived out of order but in time to be played.
    pub reordered: u64,
    /// Duplicate packets ignored.
    pub duplicates: u64,
    /// Restarts (large sequence jumps).
    pub resets: u64,
}

pub struct Reorder {
    slots: Vec<Slot>,
    /// Sequence number to release next; `None` before the first packet.
    next: Option<u16>,
    /// Highest sequence number seen, to tell reordered packets from new ones.
    highest: u16,
    held: usize,
    /// When we started waiting for the missing `next` packet.
    waiting_since: Option<Instant>,
    wait: Duration,
    pub stats: ReorderStats,
}

impl Reorder {
    /// Hold out-of-order packets for up to `wait` before declaring a loss.
    pub fn new(wait: Duration) -> Self {
        Self {
            slots: (0..SLOTS).map(|_| Slot::default()).collect(),
            next: None,
            highest: 0,
            held: 0,
            waiting_since: None,
            wait,
            stats: ReorderStats::default(),
        }
    }

    /// Accept a packet that arrived at `now`, then release everything that
    /// is ready, in order, through `out`.
    pub fn push(
        &mut self,
        seq: u16,
        payload: &[u8],
        now: Instant,
        mut out: impl FnMut(Released<'_>),
    ) {
        let Some(next) = self.next else {
            self.next = Some(seq);
            self.highest = seq;
            self.store(seq, payload);
            out(Released::Reset);
            self.release(now, false, &mut out);
            return;
        };
        let ahead = seq.wrapping_sub(next) as i16 as i32;
        if (0..MAX_JUMP).contains(&ahead) {
            let from_highest = seq.wrapping_sub(self.highest) as i16;
            if from_highest > 0 {
                self.highest = seq;
            } else if from_highest < 0 {
                self.stats.reordered += 1;
            }
            // Too far ahead to hold: give up on the oldest gap(s) now.
            while seq.wrapping_sub(self.next.unwrap()) as usize >= SLOTS {
                if self.held == 0 {
                    // Nothing in between to wait for: a long run of losses,
                    // too long to conceal.
                    self.stats.lost += seq.wrapping_sub(self.next.unwrap()) as u64;
                    self.next = Some(seq);
                    self.waiting_since = None;
                    break;
                }
                self.release_one_gap(&mut out);
            }
            self.store(seq, payload);
        } else if ahead < 0 && ahead > -MAX_JUMP {
            // Already played past it (or written it off).
            self.stats.late += 1;
            return;
        } else {
            // A jump: whatever we hold is from before it. Play it out, then
            // restart from this packet.
            self.flush(&mut out);
            self.next = Some(seq);
            self.highest = seq;
            self.stats.resets += 1;
            self.store(seq, payload);
            out(Released::Reset);
        }
        self.release(now, false, &mut out);
    }

    /// Release everything held, declaring any gaps lost (e.g. the stream
    /// went quiet, so nothing more is coming for a while).
    pub fn flush(&mut self, mut out: impl FnMut(Released<'_>)) {
        self.release(Instant::now(), true, &mut out);
    }

    /// Release what's ready at `now`. If nothing has arrived for a while,
    /// call this (or [`Reorder::flush`]) so held packets don't wait forever.
    pub fn poll(&mut self, now: Instant, mut out: impl FnMut(Released<'_>)) {
        self.release(now, false, &mut out);
    }

    fn store(&mut self, seq: u16, payload: &[u8]) {
        let slot = &mut self.slots[seq as usize % SLOTS];
        if slot.full {
            // Only the same seq can be here: anything else is >= SLOTS away.
            self.stats.duplicates += 1;
            return;
        }
        slot.seq = seq;
        slot.full = true;
        slot.payload.clear();
        slot.payload.extend_from_slice(payload);
        self.held += 1;
    }

    fn release(&mut self, now: Instant, force: bool, out: &mut impl FnMut(Released<'_>)) {
        while self.held > 0 {
            let next = self.next.expect("held packets imply a position");
            let i = next as usize % SLOTS;
            if self.slots[i].full {
                self.slots[i].full = false;
                self.held -= 1;
                self.next = Some(next.wrapping_add(1));
                self.waiting_since = None;
                out(Released::Packet(&self.slots[i].payload));
                continue;
            }
            // `next` is missing but later packets are here.
            let since = *self.waiting_since.get_or_insert(now);
            if !force && now.duration_since(since) < self.wait {
                return;
            }
            self.release_one_gap(out);
        }
    }

    /// Write off the gap at `next` and release the packet after it.
    fn release_one_gap(&mut self, out: &mut impl FnMut(Released<'_>)) {
        let next = self.next.expect("a gap implies a position");
        let count = (0..SLOTS as u16)
            .find(|&d| self.slots[next.wrapping_add(d) as usize % SLOTS].full)
            .expect("a gap is only released when later packets are held");
        let seq = next.wrapping_add(count);
        let i = seq as usize % SLOTS;
        self.slots[i].full = false;
        self.held -= 1;
        self.next = Some(seq.wrapping_add(1));
        self.waiting_since = None;
        if count > 0 {
            self.stats.lost += count as u64;
            out(Released::Lost {
                count,
                next: &self.slots[i].payload,
            });
        }
        out(Released::Packet(&self.slots[i].payload));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What came out, as compact strings: "p3" = packet with payload [3],
    /// "L2>5" = 2 lost before payload [5], "R" = reset.
    fn run(r: &mut Reorder, seq: u16, now: Instant) -> Vec<String> {
        let mut v = Vec::new();
        r.push(seq, &[seq as u8], now, |e| v.push(fmt(e)));
        v
    }

    fn fmt(e: Released<'_>) -> String {
        match e {
            Released::Packet(p) => format!("p{}", p[0]),
            Released::Lost { count, next } => format!("L{count}>{}", next[0]),
            Released::Reset => "R".into(),
        }
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn in_order_passes_straight_through() {
        let mut r = Reorder::new(ms(20));
        let t = Instant::now();
        assert_eq!(run(&mut r, 10, t), ["R", "p10"]);
        assert_eq!(run(&mut r, 11, t), ["p11"]);
        assert_eq!(run(&mut r, 12, t), ["p12"]);
        assert_eq!(r.stats, ReorderStats::default());
    }

    #[test]
    fn swapped_packets_are_put_back_in_order() {
        let mut r = Reorder::new(ms(20));
        let t = Instant::now();
        run(&mut r, 1, t);
        assert!(run(&mut r, 3, t).is_empty()); // waiting for 2
        assert_eq!(run(&mut r, 2, t + ms(5)), ["p2", "p3"]);
        assert_eq!(r.stats.reordered, 1);
        assert_eq!(r.stats.lost, 0);
    }

    #[test]
    fn missing_packet_is_written_off_after_wait() {
        let mut r = Reorder::new(ms(20));
        let t = Instant::now();
        run(&mut r, 1, t);
        assert!(run(&mut r, 3, t).is_empty());
        assert!(run(&mut r, 4, t + ms(10)).is_empty());
        assert_eq!(run(&mut r, 5, t + ms(20)), ["L1>3", "p3", "p4", "p5"]);
        assert_eq!(r.stats.lost, 1);
        // The straggler shows up after all: too late.
        assert!(run(&mut r, 2, t + ms(25)).is_empty());
        assert_eq!(r.stats.late, 1);
    }

    #[test]
    fn poll_and_flush_release_without_new_packets() {
        let mut r = Reorder::new(ms(20));
        let t = Instant::now();
        run(&mut r, 1, t);
        run(&mut r, 4, t);
        let mut v = Vec::new();
        r.poll(t + ms(5), |e| v.push(fmt(e)));
        assert!(v.is_empty());
        r.poll(t + ms(21), |e| v.push(fmt(e)));
        assert_eq!(v, ["L2>4", "p4"]);

        run(&mut r, 7, t + ms(22));
        v.clear();
        r.flush(|e| v.push(fmt(e)));
        assert_eq!(v, ["L2>7", "p7"]);
    }

    #[test]
    fn wraps_around_sequence_numbers() {
        let mut r = Reorder::new(ms(20));
        let t = Instant::now();
        run(&mut r, 65534, t);
        assert!(run(&mut r, 0, t).is_empty()); // 65535 missing
        assert_eq!(run(&mut r, 65535, t), ["p255", "p0"]);
        assert_eq!(run(&mut r, 1, t), ["p1"]);
        assert_eq!(r.stats.reordered, 1);
    }

    #[test]
    fn duplicates_are_ignored() {
        let mut r = Reorder::new(ms(20));
        let t = Instant::now();
        run(&mut r, 1, t);
        run(&mut r, 3, t);
        assert!(run(&mut r, 3, t).is_empty());
        assert_eq!(r.stats.duplicates, 1);
        assert_eq!(run(&mut r, 2, t), ["p2", "p3"]);
        // A duplicate of something already played is just late.
        assert!(run(&mut r, 2, t).is_empty());
        assert_eq!(r.stats.late, 1);
    }

    #[test]
    fn far_ahead_packet_forces_old_gaps_out() {
        let mut r = Reorder::new(Duration::from_secs(10));
        let t = Instant::now();
        run(&mut r, 0, t);
        run(&mut r, 2, t); // 1 missing, waiting

        // 200 is beyond the 128 slots past `next` (1): give up on 1 and
        // play 2. The 197 still missing before 200 are too many to hold
        // out for, so 200 plays straight away.
        assert_eq!(run(&mut r, 200, t), ["L1>2", "p2", "p200"]);
        assert_eq!(r.stats.lost, 1 + 197);
    }

    #[test]
    fn long_outage_skips_ahead_without_waiting() {
        let mut r = Reorder::new(ms(20));
        let t = Instant::now();
        run(&mut r, 0, t);
        // 1..=499 never arrive; nothing is held, so 500 plays at once.
        assert_eq!(run(&mut r, 500, t), ["p244"]);
        assert_eq!(r.stats.lost, 499);
        assert_eq!(run(&mut r, 501, t), ["p245"]);
    }

    #[test]
    fn big_jump_resets() {
        let mut r = Reorder::new(ms(20));
        let t = Instant::now();
        run(&mut r, 100, t);
        run(&mut r, 102, t); // 101 missing
        assert_eq!(run(&mut r, 40_000, t), ["L1>102", "p102", "R", "p64"]);
        assert_eq!(r.stats.resets, 1);
        assert_eq!(run(&mut r, 40_001, t), ["p65"]);
    }
}
