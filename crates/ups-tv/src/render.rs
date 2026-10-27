//! Framebuffer compositor — the "old games" layer. Everything draws as
//! blended rectangles and alpha text onto a u32 buffer; no GPU, no DOM,
//! a few hundred KB of scratch memory total.
//!
//! The buffer is always exactly the window/panel size — minifb requires
//! buffer dims == window dims, so callers pass (fw, fh) everywhere
//! instead of a fixed constant.

use ups_core::chart::Note;

// palette — dark karaoke bar
pub const BG: u32 = 0x0b0e14;
pub const LANE_BG: u32 = 0x141a26;
pub const NOTE: u32 = 0x4da3ff;
pub const GOLDEN: u32 = 0xffd24d;
pub const FREESTYLE: u32 = 0x5ad17a;
pub const PLAYHEAD: u32 = 0xffffff;
pub const SING_DOT: u32 = 0xff5a5a;
pub const TEXT: u32 = 0xf2f2f2;
pub const TEXT_DIM: u32 = 0x8a93a5;
pub const SUNG: u32 = 0xffd24d;
pub const TRAIL: u32 = 0xff6b6b;
pub const TRAIL_GLOW: u32 = 0xffd2b0;
pub const HIT_FILL: u32 = 0xfff3c4;
pub const STREAK: u32 = 0x9ae6ff;

/// one-shot particle — physics stepped by the caller, drawn here as a
/// fading square. Cheap, no sprites.
pub struct Particle {
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
    pub born: f32,
    pub life: f32,
    pub rgb: u32,
    pub grav: f32,
}

impl Particle {
    pub fn age(&self, now: f32) -> f32 {
        now - self.born
    }
    pub fn alive(&self, now: f32) -> bool {
        self.age(now) < self.life
    }
    pub fn step(&mut self, dt: f32) {
        self.vy += self.grav * dt;
        self.x += self.vx * dt;
        self.y += self.vy * dt;
    }
}

pub fn draw_particles(fb: &mut [u32], fw: usize, fh: usize,
                      ps: &[Particle], now: f32) {
    for p in ps {
        let k = 1.0 - p.age(now) / p.life;
        if k <= 0.0 {
            continue;
        }
        let a = (k * 240.0) as u32;
        let s = if k > 0.55 { 3 } else { 2 };
        rect(fb, fw, fh, p.x as i32, p.y as i32, s, s, p.rgb, a);
    }
}

/// lane geometry shared by notes/trail — (playhead_x, px_per_ms,
/// tone_min, tone_max, row_h)
pub fn lane_geo(fw: usize, h: i32, window_ms: f64)
                -> (i32, f64, f64, f64, f64) {
    let tone_min = 4.0f64;
    let tone_max = 42.0f64;
    let row_h = h as f64 / (tone_max - tone_min);
    let playhead_x = (fw as f64 * 0.22) as i32;
    let px_per_ms = fw as f64 / window_ms;
    (playhead_x, px_per_ms, tone_min, tone_max, row_h)
}

/// media_ms → lane x; tone → lane y (same mapping as draw_lane)
pub fn lane_xy(geo: (i32, f64, f64, f64, f64), y: i32, h: i32,
               ms: f64, pos_ms: f64, tone: i64) -> (i32, i32) {
    let (playhead_x, px_per_ms, tone_min, tone_max, row_h) = geo;
    let x = playhead_x + ((ms - pos_ms) * px_per_ms) as i32;
    let row = (tone as f64 - tone_min).clamp(0.0, tone_max - tone_min);
    let ty = y + h - 4 - ((row + 0.5) * row_h) as i32;
    (x, ty)
}

/// The singer's sung-tone trail — a glowing ribbon through the lane
/// made of short rect runs between consecutive pitch samples. Gaps
/// in detection break the ribbon (no phantom lines across silence).
pub fn draw_trail(fb: &mut [u32], fw: usize, fh: usize,
                  log: &[(f64, i64)], pos_ms: f64, y: i32, h: i32,
                  window_ms: f64) {
    if log.len() < 2 {
        return;
    }
    let geo = lane_geo(fw, h, window_ms);
    let mut prev: Option<(i32, i32, f64)> = None;
    // only the tail inside the window matters — find the cutoff
    let win_lo = pos_ms - geo.0 as f64 / geo.1;
    for &(ms, tone) in log {
        if ms < win_lo || ms > pos_ms + 200.0 {
            prev = None;
            continue;
        }
        let (x, ty) = lane_xy(geo, y, h, ms, pos_ms, tone);
        if let Some((px, py, pms)) = prev {
            if ms - pms < 140.0 {
                let x0 = px.min(x);
                let w = (px - x).abs().max(2);
                let y0 = py.min(ty);
                let hh = (py - ty).abs();
                // wide soft underlay then bright core
                rect(fb, fw, fh, x0, y0 - 3, w, hh + 7, TRAIL, 55);
                rect(fb, fw, fh, x0, y0 - 1, w, hh + 3, TRAIL_GLOW,
                     235);
            }
        }
        prev = Some((x, ty, ms));
    }
}

pub fn fill(fb: &mut [u32], rgb: u32) {
    fb.iter_mut().for_each(|p| *p = rgb);
}

/// blit an rgb24 frame into the u32 framebuffer
pub fn blit_video(fb: &mut [u32], rgb: &[u8], fw: usize, fh: usize) {
    debug_assert_eq!(fb.len(), fw * fh);
    debug_assert_eq!(rgb.len(), fw * fh * 3);
    for (px, c) in fb.iter_mut().zip(rgb.chunks_exact(3)) {
        *px = (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32;
    }
}

/// alpha-blended rectangle with rounded corners — per-row inset from
/// the quarter-circle at each end (the lyric band's card)
pub fn rrect(fb: &mut [u32], fw: usize, fh: usize,
             x: i32, y: i32, w: i32, h: i32, rad: i32, rgb: u32, a: u32) {
    let r = rad.max(0).min(w / 2).min(h / 2);
    for yy in y..(y + h) {
        let edge = (yy - y).min((y + h - 1) - yy);
        let inset = if edge < r {
            let d = (r - edge) as f64;
            r - ((r * r) as f64 - d * d).max(0.0).sqrt() as i32
        } else {
            0
        };
        rect(fb, fw, fh, x + inset, yy, w - 2 * inset, 1, rgb, a);
    }
}

/// alpha-blended rectangle (a = 0..255)
pub fn rect(fb: &mut [u32], fw: usize, fh: usize,
            x: i32, y: i32, w: i32, h: i32, rgb: u32, a: u32) {
    let x0 = x.max(0);
    let y0 = y.max(0);
    let x1 = (x + w).min(fw as i32);
    let y1 = (y + h).min(fh as i32);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let (r, g, b) =
        ((rgb >> 16) & 0xff, (rgb >> 8) & 0xff, rgb & 0xff);
    let inv = 255 - a;
    for yy in y0..y1 {
        let row = yy as usize * fw;
        for xx in x0..x1 {
            let i = row + xx as usize;
            let d = fb[i];
            fb[i] = ((r * a + ((d >> 16) & 0xff) * inv) / 255) << 16
                | ((g * a + ((d >> 8) & 0xff) * inv) / 255) << 8
                | ((b * a + (d & 0xff) * inv) / 255);
        }
    }
}

/// One pitch lane for a track: notes scroll right→left across a
/// `window_ms`-wide view; `pos_ms` sits at the playhead column.
/// Flair: gloss strip on pills, the already-sung span of the active
/// note fills white-hot, and the playhead carries a soft glow column.
pub fn draw_lane(fb: &mut [u32], fw: usize, fh: usize,
                 notes: &[Note], gap_ms: f64,
                 beat_ms: f64, pos_ms: f64, y: i32, h: i32,
                 window_ms: f64, sung: Option<i64>,
                 hit_flash: bool) {
    rect(fb, fw, fh, 0, y, fw as i32, h, LANE_BG, 150);
    let (playhead_x, px_per_ms, tone_min, tone_max, row_h) =
        lane_geo(fw, h, window_ms);

    for n in notes {
        if n.kind == "freestyle" && n.text.is_empty() {
            continue;
        }
        let n_start = gap_ms + n.start as f64 * beat_ms;
        let n_end = gap_ms + (n.start + n.length) as f64 * beat_ms;
        if n_end < pos_ms - playhead_x as f64 / px_per_ms
            || n_start > pos_ms + (fw as f64 - playhead_x as f64) / px_per_ms
        {
            continue;
        }
        let x = playhead_x + ((n_start - pos_ms) * px_per_ms) as i32;
        let w = ((n_end - n_start) * px_per_ms) as i32;
        let row = (n.pitch as f64 - tone_min).clamp(0.0, tone_max - tone_min);
        let ny = y + h - 4 - ((row + 1.0) * row_h) as i32;
        let rh = row_h as i32;
        let (color, gold) = match n.kind.as_str() {
            "golden" | "rapgolden" => (GOLDEN, true),
            "freestyle" => (FREESTYLE, false),
            "rap" => (0xc792ff, false),
            _ => (NOTE, false),
        };
        rect(fb, fw, fh, x, ny, w, rh, color, 235);
        // gloss strip — top edge highlight, like the web pill sheen
        rect(fb, fw, fh, x, ny, w, 2, 0xffffff, 70);
        // sung fill — the span already behind the playhead fills
        // white-hot while the note is active
        if pos_ms >= n_start && pos_ms < n_end {
            let sung_w = (playhead_x - x).clamp(0, w);
            let fill_col = if gold { 0xfff3c4 } else { 0xcfe6ff };
            rect(fb, fw, fh, x, ny + 1, sung_w, rh - 2, fill_col, 210);
        }
    }
    // "now" column — wide soft glow + bright core
    rect(fb, fw, fh, playhead_x - 12, y, 24, h, GOLDEN, 26);
    rect(fb, fw, fh, playhead_x - 1, y, 2, h, PLAYHEAD, 255);
    // sung tone dot — bright core with a small halo
    if let Some(t) = sung {
        let row = (t as f64 - tone_min).clamp(0.0, tone_max - tone_min);
        let dy = y + h - 4 - ((row + 0.5) * row_h) as i32;
        rect(fb, fw, fh, playhead_x - 9, dy - 8, 18, 16, SING_DOT, 60);
        rect(fb, fw, fh, playhead_x - 6, dy - 5, 12, 10, SING_DOT, 255);
    }
    if hit_flash {
        rect(fb, fw, fh, 0, y, 6, h, GOLDEN, 200);
    }
}
