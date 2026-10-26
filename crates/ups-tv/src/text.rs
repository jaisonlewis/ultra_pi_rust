//! Bitmap text — fontdue glyph rasterization into a small cache,
//! alpha-composited onto the u32 framebuffer. Old-school style: no GPU,
//! no atlas allocator, just blit-and-go.

use std::collections::HashMap;

use fontdue::{Font, FontSettings};

pub struct Text {
    font: Font,
    cache: HashMap<(char, u32), (Vec<u8>, usize, usize, i32, i32)>,
}

pub fn find_font() -> Option<Vec<u8>> {
    for p in [
        // Windows
        "C:/Windows/Fonts/segoeui.ttf",
        "C:/Windows/Fonts/arial.ttf",
        // Debian/Pi
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
        // macOS
        "/System/Library/Fonts/Helvetica.ttc",
    ] {
        if let Ok(bytes) = std::fs::read(p) {
            return Some(bytes);
        }
    }
    None
}

impl Text {
    pub fn new() -> Option<Self> {
        let bytes = find_font()?;
        let font = Font::from_bytes(bytes, FontSettings::default()).ok()?;
        Some(Text { font, cache: HashMap::new() })
    }

    /// Rasterize `ch` at `px` (cached); returns (alpha, w, h, xmin, ymin).
    fn glyph(&mut self, ch: char, px: u32) -> &(Vec<u8>, usize, usize, i32, i32) {
        self.cache.entry((ch, px)).or_insert_with(|| {
            let (m, bmp) = self.font.rasterize(ch, px as f32);
            (bmp, m.width, m.height, m.xmin, m.ymin)
        })
    }

    /// Advance width — same integer truncation as draw() so centering
    /// math agrees with what's actually painted.
    pub fn width(&mut self, s: &str, px: u32) -> usize {
        s.chars()
            .map(|c| {
                self.font.metrics(c, px as f32).advance_width as i32
            })
            .sum::<i32>()
            .max(0) as usize
    }

    /// Draw `s` with its line-top at x,y with rgb. Returns advance used.
    pub fn draw(&mut self, fb: &mut [u32], fw: usize, fh: usize,
                s: &str, x: i32, y: i32, px: u32, rgb: u32) -> usize {
        let mut cx = x;
        let r = ((rgb >> 16) & 0xff) as u32;
        let g = ((rgb >> 8) & 0xff) as u32;
        let b = (rgb & 0xff) as u32;
        // ymin is the baseline→bottom-edge offset (negative = glyph
        // descends below the baseline); bitmap top = baseline - ymin - h
        let baseline = y
            + self
                .font
                .horizontal_line_metrics(px as f32)
                .map(|m| m.ascent.ceil() as i32)
                .unwrap_or(px as i32);
        for ch in s.chars() {
            let adv = self.font.metrics(ch, px as f32).advance_width as i32;
            if ch != ' ' {
                let (bmp, gw, gh, xmin, ymin) = self.glyph(ch, px).clone();
                for gy in 0..gh {
                    let dy = baseline - ymin - gh as i32 + gy as i32;
                    if dy < 0 || dy >= fh as i32 {
                        continue;
                    }
                    for gx in 0..gw {
                        let dx = cx + gx as i32 + xmin;
                        if dx < 0 || dx >= fw as i32 {
                            continue;
                        }
                        let a = bmp[gy * gw + gx] as u32;
                        if a == 0 {
                            continue;
                        }
                        let i = dy as usize * fw + dx as usize;
                        let dst = fb[i];
                        let dr = ((dst >> 16) & 0xff) as u32;
                        let dg = ((dst >> 8) & 0xff) as u32;
                        let db = (dst & 0xff) as u32;
                        let inv = 255 - a;
                        fb[i] = ((r * a + dr * inv) / 255) << 16
                            | ((g * a + dg * inv) / 255) << 8
                            | ((b * a + db * inv) / 255);
                    }
                }
            }
            cx += adv;
        }
        (cx - x) as usize
    }
}
