//! Resampler throughput: `cargo run --release -p mcast-audio --example resample_bench`
use mcast_audio::convert::Converter;
use std::time::Instant;
fn main() {
    for (from, to, ch) in [
        (44_100u32, 48_000u32, 2u16),
        (48_000, 8_000, 1),
        (48_000, 16_000, 1),
        (48_000, 44_100, 2),
    ] {
        let secs = 10.0;
        let n = (from as f64 * secs) as usize * ch as usize;
        let input: Vec<f32> = (0..n).map(|i| ((i as f32) * 0.01).sin() * 0.5).collect();
        let mut c = Converter::new(from, ch, to, ch);
        let mut out = Vec::with_capacity(n * 2);
        let t = Instant::now();
        for chunk in input.chunks(480 * ch as usize) {
            c.process(chunk, &mut out);
        }
        let e = t.elapsed();
        println!(
            "{from}->{to} x{ch}: {:.1} ms per 10 s of audio ({:.0}x realtime)",
            e.as_secs_f64() * 1e3,
            secs / e.as_secs_f64()
        );
    }
}
