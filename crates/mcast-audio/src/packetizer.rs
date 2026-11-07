//! Turns a stream of interleaved samples into fixed-size RTP packets.

use crate::format::AudioFormat;
use mcast_core::rtp::RtpHeader;

pub struct Packetizer {
    format: AudioFormat,
    frames_per_packet: usize,
    payload_type: u8,
    ssrc: u32,
    sequence: u16,
    timestamp: u32,
    pending: Vec<f32>,
    packet: Vec<u8>,
    first: bool,
}

impl Packetizer {
    pub fn new(format: AudioFormat, frames_per_packet: usize, payload_type: u8, ssrc: u32) -> Self {
        Self {
            format,
            frames_per_packet,
            payload_type,
            ssrc,
            sequence: random_u32() as u16,
            timestamp: random_u32(),
            pending: Vec::new(),
            packet: Vec::new(),
            first: true,
        }
    }

    pub fn frames_per_packet(&self) -> usize {
        self.frames_per_packet
    }

    /// Queue samples and call `emit` for every complete packet.
    pub fn push(&mut self, samples: &[f32], mut emit: impl FnMut(&[u8])) {
        self.pending.extend_from_slice(samples);
        let chunk = self.frames_per_packet * self.format.channels as usize;
        let mut offset = 0;
        while self.pending.len() - offset >= chunk {
            self.packet.clear();
            RtpHeader {
                marker: self.first,
                payload_type: self.payload_type,
                sequence: self.sequence,
                timestamp: self.timestamp,
                ssrc: self.ssrc,
            }
            .write_to(&mut self.packet);
            self.format
                .encode(&self.pending[offset..offset + chunk], &mut self.packet);
            emit(&self.packet);

            self.first = false;
            self.sequence = self.sequence.wrapping_add(1);
            self.timestamp = self.timestamp.wrapping_add(self.frames_per_packet as u32);
            offset += chunk;
        }
        self.pending.drain(..offset);
    }
}

/// Non-cryptographic random value for SSRC / initial sequence & timestamp.
pub fn random_u32() -> u32 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut h = RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    h.finish() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use mcast_core::rtp;

    #[test]
    fn emits_full_packets_only() {
        let fmt = AudioFormat::default(); // stereo L16
        let mut p = Packetizer::new(fmt, 4, 96, 42);
        let mut packets = Vec::new();
        p.push(&[0.0; 6], |b| packets.push(b.to_vec())); // 3 frames: none yet
        assert!(packets.is_empty());
        p.push(&[0.0; 12], |b| packets.push(b.to_vec())); // 9 frames total: 2 packets
        assert_eq!(packets.len(), 2);

        let a = rtp::parse(&packets[0]).unwrap();
        let b = rtp::parse(&packets[1]).unwrap();
        assert!(a.header.marker && !b.header.marker);
        assert_eq!(a.payload.len(), 4 * fmt.frame_bytes());
        assert_eq!(b.header.sequence, a.header.sequence.wrapping_add(1));
        assert_eq!(b.header.timestamp, a.header.timestamp.wrapping_add(4));
        assert_eq!(a.header.ssrc, 42);
    }
}
