//! Framebuffer compositor — the "old games" layer. Everything draws as
//! blended rectangles and alpha text onto a u32 buffer; no GPU, no DOM,
//! a few hundred KB of scratch memory total.

use ups_core::chart::Note;

pub const W: usize = 1280;
pub const H: usize = 720;

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

pub fn fill(fb: &mut [u32], rgb: u32) {
    fb.iter_mut().for_each(|p| *p = rgb);
}

/// blit an rgb24 frame into the u32 framebuffer
pub fn blit_video(fb: &mut [u32], rgb: &[u8]) {
    debug_assert_eq!(fb.len(), W * H);
    debug_assert_eq!(rgb.len(), W * H * 3);
    for (px, c) in fb.iter_mut().zip(rgb.chunks_exact(3)) {
        *px = (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32;
    }
}

/// alpha-blended rectangle (a = 0..255)
pub fn rect(fb: &mut [u32], x: i32, y: i32, w: i32, h: i32,
            rgb: u32, a: u32) {
    let x0 = x.max(0);
    let y0 = y.max(0);
    let x1 = (x + w).min(W as i32);
    let y1 = (y + h).min(H as i32);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let (r, g, b) =
        ((rgb >> 16) & 0xff, (rgb >> 8) & 0xff, rgb & 0xff);
    let inv = 255 - a;
    for yy in y0..y1 {
        let row = yy as usize * W;
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
/// Returns the sung-tone row hit for the flash.
pub fn draw_lane(fb: &mut [u32], notes: &[Note], gap_ms: f64,
                 beat_ms: f64, pos_ms: f64, y: i32, h: i32,
                 window_ms: f64, sung: Option<i64>,
                 hit_flash: bool) {
    rect(fb, 0, y, W as i32, h, LANE_BG, 220);
    // tone range C2..A5 = 0..45; squash a bit for display
    let tone_min = 4.0f64;
    let tone_max = 42.0f64;
    let row_h = h as f64 / (tone_max - tone_min);
    let playhead_x = (W as f64 * 0.22) as i32;
    let px_per_ms = W as f64 / window_ms;

    for n in notes {
        if n.kind == "freestyle" && n.text.is_empty() {
            continue;
        }
        let n_start = gap_ms + n.start as f64 * beat_ms;
        let n_end = gap_ms + (n.start + n.length) as f64 * beat_ms;
        if n_end < pos_ms - playhead_x as f64 / px_per_ms
            || n_start > pos_ms + (W as f64 - playhead_x as f64) / px_per_ms
        {
            continue;
        }
        let x = playhead_x + ((n_start - pos_ms) * px_per_ms) as i32;
        let w = ((n_end - n_start) * px_per_ms) as i32;
        let row = (n.pitch as f64 - tone_min).clamp(0.0, tone_max - tone_min);
        let ny = y + h - 4 - ((row + 1.0) * row_h) as i32;
        let color = match n.kind.as_str() {
            "golden" => GOLDEN,
            "freestyle" => FREESTYLE,
            "rap" | "rapgolden" => 0xc792ff,
            _ => NOTE,
        };
        rect(fb, x, ny, w, row_h as i32, color, 255);
    }
    // playhead
    rect(fb, playhead_x - 1, y, 2, h, PLAYHEAD, 255);
    // sung tone dot
    if let Some(t) = sung {
        let row = (t as f64 - tone_min).clamp(0.0, tone_max - tone_min);
        let dy = y + h - 4 - ((row + 0.5) * row_h) as i32;
        rect(fb, playhead_x - 6, dy - 5, 12, 10, SING_DOT, 255);
    }
    if hit_flash {
        rect(fb, 0, y, 6, h, GOLDEN, 200);
    }
}
