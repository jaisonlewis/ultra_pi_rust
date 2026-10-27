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

/// Bold face for lyrics/score — the web TV renders the lyric line at
/// weight 700. Falls back to the regular face when no bold is around.
pub fn find_font_bold() -> Option<Vec<u8>> {
    for p in [
        "C:/Windows/Fonts/segoeuib.ttf",
        "C:/Windows/Fonts/arialbd.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
        "/System/Library/Fonts/Helvetica Bold.ttf",
    ] {
        if let Ok(bytes) = std::fs::read(p) {
            return Some(bytes);
        }
    }
    find_font()
}

impl Text {
    pub fn new() -> Option<Self> {
        let bytes = find_font()?;
        let font = Font::from_bytes(bytes, FontSettings::default()).ok()?;
        Some(Text { font, cache: HashMap::new() })
    }

    pub fn new_bold() -> Option<Self> {
        let bytes = find_font_bold()?;
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

    fn baseline(&self, y: i32, px: u32) -> i32 {
        y + self
            .font
            .horizontal_line_metrics(px as f32)
            .map(|m| m.ascent.ceil() as i32)
            .unwrap_or(px as i32)
    }

    /// Blit one glyph bitmap alpha-composited in `rgb`, origin at
    /// (cx, baseline), skipping pixels at/ right of `clip_x`
    /// (i32::MAX = no clip — used for the karaoke wipe).
    #[allow(clippy::too_many_arguments)]
    fn blit(fb: &mut [u32], fw: usize, fh: usize,
            bmp: &[u8], gw: usize, gh: usize, xmin: i32, ymin: i32,
            cx: i32, baseline: i32, rgb: u32, clip_x: i32) {
        let r = ((rgb >> 16) & 0xff) as u32;
        let g = ((rgb >> 8) & 0xff) as u32;
        let b = (rgb & 0xff) as u32;
        for gy in 0..gh {
            let dy = baseline - ymin - gh as i32 + gy as i32;
            if dy < 0 || dy >= fh as i32 {
                continue;
            }
            for gx in 0..gw {
                let dx = cx + gx as i32 + xmin;
                if dx < 0 || dx >= fw as i32 || dx >= clip_x {
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

    /// Per-glyph iteration shared by every draw variant: calls
    /// `f(cx, baseline, bmp, gw, gh, xmin, ymin)` for each non-space
    /// glyph, advancing cx by the font metric. Returns total advance.
    fn each_glyph(&mut self, s: &str, x: i32, y: i32, px: u32,
                  mut f: impl FnMut(i32, i32, &[u8], usize, usize, i32, i32))
        -> usize {
        let mut cx = x;
        let baseline = self.baseline(y, px);
        for ch in s.chars() {
            let adv = self.font.metrics(ch, px as f32).advance_width as i32;
            if ch != ' ' {
                let (bmp, gw, gh, xmin, ymin) = self.glyph(ch, px).clone();
                f(cx, baseline, &bmp, gw, gh, xmin, ymin);
            }
            cx += adv;
        }
        (cx - x) as usize
    }

    /// Draw `s` with its line-top at x,y with rgb. Returns advance used.
    pub fn draw(&mut self, fb: &mut [u32], fw: usize, fh: usize,
                s: &str, x: i32, y: i32, px: u32, rgb: u32) -> usize {
        self.each_glyph(s, x, y, px, |cx, base, bmp, gw, gh, xm, ym| {
            Self::blit(fb, fw, fh, bmp, gw, gh, xm, ym, cx, base,
                       rgb, i32::MAX);
        })
    }

    /// Fill pass clipped at `clip_x` — pixels at/right of the wipe
    /// edge are skipped, so a second pass in another color produces
    /// the karaoke left→right fill. Returns advance used.
    pub fn draw_clip(&mut self, fb: &mut [u32], fw: usize, fh: usize,
                     s: &str, x: i32, y: i32, px: u32, rgb: u32,
                     clip_x: i32) -> usize {
        self.each_glyph(s, x, y, px, |cx, base, bmp, gw, gh, xm, ym| {
            Self::blit(fb, fw, fh, bmp, gw, gh, xm, ym, cx, base,
                       rgb, clip_x);
        })
    }

    /// Stroke pass — the same glyphs painted in dark `rgb` at eight
    /// offsets, i.e. a cheap outline. Draw the fill(s) on top after.
    /// `o` is the offset in px (1–2 at lyric sizes).
    pub fn draw_outline(&mut self, fb: &mut [u32], fw: usize, fh: usize,
                        s: &str, x: i32, y: i32, px: u32, rgb: u32,
                        o: i32) -> usize {
        self.each_glyph(s, x, y, px, |cx, base, bmp, gw, gh, xm, ym| {
            for (ox, oy) in [
                (-o, -o), (0, -o), (o, -o),
                (-o, 0),           (o, 0),
                (-o, o),  (0, o),  (o, o),
            ] {
                Self::blit(fb, fw, fh, bmp, gw, gh, xm, ym, cx + ox,
                           base + oy, rgb, i32::MAX);
            }
        })
    }
}
