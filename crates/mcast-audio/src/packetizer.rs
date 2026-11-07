//! Cuts a stream of interleaved samples into fixed-duration RTP packets.

use crate::codec::{Encoder, StreamSpec};
use mcast_core::random_u32;
use mcast_core::rtp::RtpHeader;

pub struct Packetizer {
    channels: usize,
    frames_per_packet: usize,
    /// RTP timestamp increment per packet (in the codec's RTP clock).
    ts_step: u32,
    encoder: Box<dyn Encoder>,
    payload_type: u8,
    ssrc: u32,
    sequence: u16,
    timestamp: u32,
    pending: Vec<f32>,
    packet: Vec<u8>,
    first: bool,
}

impl Packetizer {
    pub fn new(
        spec: StreamSpec,
        encoder: Box<dyn Encoder>,
        frames_per_packet: usize,
        payload_type: u8,
        ssrc: u32,
    ) -> Self {
        let ts_step = (frames_per_packet as u64 * spec.rtp_clock_rate() as u64
            / spec.sample_rate as u64) as u32;
        Self {
            channels: spec.channels as usize,
            frames_per_packet,
            ts_step,
            encoder,
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
    pub fn push(&mut self, samples: &[f32], mut emit: impl FnMut(&[u8])) -> Result<(), String> {
        self.pending.extend_from_slice(samples);
        let chunk = self.frames_per_packet * self.channels;
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
            self.encoder
                .encode(&self.pending[offset..offset + chunk], &mut self.packet)?;
            emit(&self.packet);

            self.first = false;
            self.sequence = self.sequence.wrapping_add(1);
            self.timestamp = self.timestamp.wrapping_add(self.ts_step);
            offset += chunk;
        }
        self.pending.drain(..offset);
        Ok(())
    }

    /// Pad any partial packet with silence and send it.
    pub fn finish(&mut self, emit: impl FnMut(&[u8])) -> Result<(), String> {
        let chunk = self.frames_per_packet * self.channels;
        let partial = self.pending.len() % chunk;
        if partial == 0 {
            return Ok(());
        }
        let pad = vec![0.0; chunk - partial];
        self.push(&pad, emit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{Codec, OpusOptions};
    use mcast_core::rtp;

    fn packetizer(spec: StreamSpec, frames: usize) -> Packetizer {
        Packetizer::new(
            spec,
            spec.encoder(&OpusOptions::default()).unwrap(),
            frames,
            96,
            42,
        )
    }

    #[test]
    fn emits_full_packets_only() {
        let spec = StreamSpec::new(Codec::L16, 48_000, 2);
        let mut p = packetizer(spec, 4);
        let mut packets = Vec::new();
        p.push(&[0.0; 6], |b| packets.push(b.to_vec())).unwrap(); // 3 frames: none yet
        assert!(packets.is_empty());
        p.push(&[0.0; 12], |b| packets.push(b.to_vec())).unwrap(); // 9 frames: 2 packets
        assert_eq!(packets.len(), 2);
        p.finish(|b| packets.push(b.to_vec())).unwrap(); // 1 frame left, padded
        assert_eq!(packets.len(), 3);

        let a = rtp::parse(&packets[0]).unwrap();
        let b = rtp::parse(&packets[1]).unwrap();
        assert!(a.header.marker && !b.header.marker);
        assert_eq!(a.payload.len(), 4 * 4);
        assert_eq!(b.header.sequence, a.header.sequence.wrapping_add(1));
        assert_eq!(b.header.timestamp, a.header.timestamp.wrapping_add(4));
        assert_eq!(a.header.ssrc, 42);
    }

    #[test]
    fn g722_uses_8khz_rtp_clock() {
        let spec = StreamSpec::new(Codec::G722, 0, 0);
        let mut p = packetizer(spec, 320); // 20 ms at 16 kHz
        let mut packets = Vec::new();
        p.push(&[0.0; 640], |b| packets.push(b.to_vec())).unwrap();
        let a = rtp::parse(&packets[0]).unwrap();
        let b = rtp::parse(&packets[1]).unwrap();
        assert_eq!(a.payload.len(), 160);
        assert_eq!(b.header.timestamp.wrapping_sub(a.header.timestamp), 160);
    }
}
