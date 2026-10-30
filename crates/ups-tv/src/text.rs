//! Bitmap text — fontdue glyph rasterization into a small cache,
//! alpha-composited onto the u32 framebuffer. Old-school style: no GPU,
//! no atlas allocator, just blit-and-go.
//!
//! Fallback chain: the primary UI font first, then every .ttf/.otf
//! found in a `fonts/` dir next to the executable (or the fixed
//! candidates below) — CJK lives there. Per-glyph, the first face
//! that actually contains the character wins, matching how a browser
//! falls through font-family.

use std::collections::HashMap;

use fontdue::{Font, FontSettings};

pub struct Text {
    fonts: Vec<Font>,
    cache: HashMap<(char, u32), (Vec<u8>, usize, usize, i32, i32)>,
}

fn primary_font() -> Option<Vec<u8>> {
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

fn bold_font() -> Option<Vec<u8>> {
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
    None
}

/// Candidate dirs scanned for fallback faces — anything *.ttf/.otf in
/// them joins the chain sorted by name. Drop Noto CJK etc. into
/// `fonts/` next to the binary and it renders.
fn font_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(e) = std::env::var("UPS_FONTS_DIR") {
        if !e.is_empty() {
            dirs.push(e.into());
        }
    }
    if let Ok(e) = std::env::var("UPS_MODELS_DIR") {
        if !e.is_empty() {
            dirs.push(format!("{e}/fonts").into());
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            dirs.push(d.join("fonts"));
            dirs.push(d.join("models/fonts"));
            // source checkout layout: <repo>/target/release/ups-app
            if let Some(repo) = d.parent().and_then(|t| t.parent()).and_then(|t| t.parent()) {
                dirs.push(repo.join("fonts"));
                dirs.push(repo.join("models/fonts"));
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        dirs.push(cwd.join("fonts"));
        dirs.push(cwd.join("models/fonts"));
    }
    dirs.push("/home/jaison/ups-rust/fonts".into());
    dirs.push("/usr/share/fonts/ups".into());
    dirs
}

/// (primary, fallbacks) font byte blobs. `bold` picks the 700 face
/// for the primary; fallbacks are shared — no CJK bold on the Pi,
/// the regular faces still render.
fn font_blobs(bold: bool) -> Vec<Vec<u8>> {
    let mut blobs = Vec::new();
    if let Some(b) = if bold { bold_font().or_else(primary_font) } else { primary_font() } {
        blobs.push(b);
    }
    let mut extra: Vec<std::path::PathBuf> = Vec::new();
    for d in font_dirs() {
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                let p = e.path();
                let ext = p.extension()
                    .and_then(|x| x.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                if ext == "ttf" || ext == "otf" {
                    extra.push(p);
                }
            }
        }
    }
    extra.sort();
    extra.dedup();
    for p in extra {
        if let Ok(b) = std::fs::read(&p) {
            blobs.push(b);
        }
    }
    blobs
}

impl Text {
    pub fn new() -> Option<Self> {
        Self::load(false)
    }

    pub fn new_bold() -> Option<Self> {
        Self::load(true)
    }

    fn load(bold: bool) -> Option<Self> {
        let fonts: Vec<Font> = font_blobs(bold)
            .into_iter()
            .filter_map(|b| Font::from_bytes(b, FontSettings::default()).ok())
            .collect();
        if fonts.is_empty() {
            return None;
        }
        Some(Text { fonts, cache: HashMap::new() })
    }

    /// First face containing `ch` — index 0 is the primary.
    fn face_for(&self, ch: char) -> &Font {
        self.fonts
            .iter()
            .find(|f| f.lookup_glyph_index(ch) != 0)
            .unwrap_or(&self.fonts[0])
    }

    /// Rasterize `ch` at `px` (cached); returns (alpha, w, h, xmin, ymin).
    fn glyph(&mut self, ch: char, px: u32) -> &(Vec<u8>, usize, usize, i32, i32) {
        if !self.cache.contains_key(&(ch, px)) {
            let (m, bmp) = self.face_for(ch).rasterize(ch, px as f32);
            self.cache.insert((ch, px), (bmp, m.width, m.height, m.xmin, m.ymin));
        }
        &self.cache[&(ch, px)]
    }

    /// Advance width — same integer truncation as draw() so centering
    /// math agrees with what's actually painted.
    pub fn width(&mut self, s: &str, px: u32) -> usize {
        s.chars()
            .map(|c| {
                self.face_for(c).metrics(c, px as f32).advance_width as i32
            })
            .sum::<i32>()
            .max(0) as usize
    }

    fn baseline(&self, y: i32, px: u32) -> i32 {
        y + self.fonts[0]
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
            let adv = self.face_for(ch).metrics(ch, px as f32).advance_width as i32;
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
