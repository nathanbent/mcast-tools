//! Show a picture in the terminal: each character cell is two pixels, the
//! top one as the foreground of "▀" and the bottom one as its background,
//! in 24-bit colour (supported by nearly every modern terminal).

use image::imageops::FilterType;
use image::GenericImageView;
use std::fmt::Write;

/// Decode `data` as an image (PNG, JPEG, GIF, WebP, BMP). `None` if it isn't one.
pub fn decode(data: &[u8]) -> Option<image::DynamicImage> {
    image::load_from_memory(data).ok()
}

/// Render to at most `max_cols` columns and `max_rows` text rows.
pub fn render(img: &image::DynamicImage, max_cols: u32, max_rows: u32) -> String {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return String::new();
    }
    // Two pixels per row; terminal cells are about twice as tall as wide,
    // so one pixel per column and two per row keeps the aspect ratio.
    let scale = (max_cols.max(1) as f64 / w as f64)
        .min(max_rows.max(1) as f64 * 2.0 / h as f64)
        .min(1.0);
    let cols = ((w as f64 * scale).round() as u32).max(1);
    let rows2 = (((h as f64 * scale).round() as u32).max(2) + 1) & !1;
    let small = img
        .resize_exact(cols, rows2, FilterType::Triangle)
        .to_rgba8();
    // Blend transparency onto black.
    let px = |x: u32, y: u32| {
        let p = small.get_pixel(x, y).0;
        let a = p[3] as u32;
        [
            (p[0] as u32 * a / 255) as u8,
            (p[1] as u32 * a / 255) as u8,
            (p[2] as u32 * a / 255) as u8,
        ]
    };
    let mut out = String::with_capacity((cols * rows2 / 2 * 40) as usize);
    for y in (0..rows2).step_by(2) {
        for x in 0..cols {
            let [r1, g1, b1] = px(x, y);
            let [r2, g2, b2] = px(x, y + 1);
            let _ = write!(out, "\x1b[38;2;{r1};{g1};{b1}m\x1b[48;2;{r2};{g2};{b2}m▀");
        }
        out.push_str("\x1b[0m\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_a_png() {
        // A 40x20 image: red left half, blue right half.
        let img = image::RgbImage::from_fn(40, 20, |x, _| {
            if x < 20 {
                image::Rgb([255, 0, 0])
            } else {
                image::Rgb([0, 0, 255])
            }
        });
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let decoded = decode(&png).unwrap();
        let text = render(&decoded, 20, 100);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 5); // 40x20 scaled to 20 columns: 10 pixel rows, 5 text rows
        assert_eq!(lines[0].matches('▀').count(), 20);
        assert!(lines[0].starts_with("\x1b[38;2;255;0;0m"));
        assert!(lines[0].contains("\x1b[38;2;0;0;255m"));
        assert!(decode(b"not an image").is_none());
    }
}
