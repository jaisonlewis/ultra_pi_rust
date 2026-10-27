//! Audio-only visualizer — the kaleidoscopic scene the browser TV shows
//! for mp3s (tv.js drawViz2D): radial spectrum spokes, two
//! counter-rotating waveform rings, breathing core, motion trails.
//! Driven by an FFT over the preloaded PCM at the playhead — perfectly
//! locked to the master clock since it analyzes the same buffer rodio
//! plays.

const N: usize = 2048; // FFT window (~46ms @44.1k)
const SPOKES: usize = 96;
const RING_PTS: usize = 160;

pub struct Viz {
    pub pulse: f32,
    pub hue: f32,
    pub phase: f32,
    /// reusable FFT workspace — no per-frame allocs
    re: Vec<f32>,
    im: Vec<f32>,
    win: Vec<f32>,
}

/// (spokes 0..1 per band, wave -1..1 samples, bass pulse 0..1)
pub struct Bands {
    pub spokes: [f32; SPOKES],
    pub wave: [f32; RING_PTS],
    pub pulse: f32,
}

impl Viz {
    pub fn new() -> Self {
        // Hann window — the analyser's 0.78 smoothing stand-in happens
        // in pulse instead
        let win = (0..N)
            .map(|i| {
                0.5 - 0.5
                    * (2.0 * std::f32::consts::PI * i as f32
                        / (N - 1) as f32)
                        .cos()
            })
            .collect();
        Viz {
            pulse: 0.0,
            hue: 0.0,
            phase: 0.0,
            re: vec![0.0; N],
            im: vec![0.0; N],
            win,
        }
    }

    /// FFT the N-sample mono window centered on `pos_ms` in `pcm`
    /// (interleaved, `ch` channels at `rate`). Out-of-range tail just
    /// zero-pads — same as the analyser at track end.
    pub fn analyze(&mut self, pcm: &[f32], ch: usize, rate: u32,
                   pos_ms: f64) -> Bands {
        let center =
            (pos_ms / 1000.0 * rate as f64 * ch as f64) as isize;
        let start = center - (N / 2 * ch) as isize;
        for i in 0..N {
            let idx = start + (i * ch) as isize;
            let s = if idx >= 0 && (idx as usize) < pcm.len() {
                // mixdown — average whatever channels exist
                let mut acc = 0.0f32;
                for c in 0..ch {
                    let j = idx as usize + c;
                    if j < pcm.len() {
                        acc += pcm[j];
                    }
                }
                acc / ch as f32
            } else {
                0.0
            };
            self.re[i] = s * self.win[i];
            self.im[i] = 0.0;
        }
        fft(&mut self.re, &mut self.im);

        // magnitudes, normalized against this window's peak bin so quiet
        // passages still draw spokes instead of going dark
        let nb = N / 2;
        let mut mag = [0f32; 512];
        let mut peak = 1e-6f32;
        for i in 1..nb.min(512) {
            let m = (self.re[i] * self.re[i] + self.im[i] * self.im[i])
                .sqrt();
            mag[i] = m;
            if m > peak {
                peak = m;
            }
        }
        // usable spectrum: bins 2..~370 (~8kHz @44.1k) -> 96 spokes
        let mut spokes = [0f32; SPOKES];
        for (i, sp) in spokes.iter_mut().enumerate() {
            let bin = 2 + (i * 368 / SPOKES);
            *sp = (mag[bin] / peak).sqrt().min(1.0);
        }
        // bass pulse — bins 2..12 ≈ 40..500Hz, absolute-scaled
        let bass: f32 = (2..12).map(|i| mag[i]).sum::<f32>() / 10.0;
        let pulse = (bass / 60.0).min(1.0);
        self.pulse = self.pulse * 0.82 + pulse * 0.18;

        // time-domain ring — raw samples decimated to RING_PTS
        let mut wave = [0f32; RING_PTS];
        for (i, w) in wave.iter_mut().enumerate() {
            let idx = start + (i * (N / RING_PTS) * ch) as isize;
            *w = if idx >= 0 && (idx as usize) < pcm.len() {
                pcm[idx as usize]
            } else {
                0.0
            };
        }
        Bands { spokes, wave, pulse: self.pulse }
    }

    /// advance hue/phase like the JS driver (per presented frame)
    pub fn step(&mut self) {
        self.hue = (self.hue + 0.25 + self.pulse * 1.6) % 360.0;
        self.phase += 0.004 + self.pulse * 0.05;
    }
}

/// iterative radix-2 FFT in place, re/im len must be power of two
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    // bit-reversal
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * std::f32::consts::PI / len as f32;
        let (wr, wi) = (ang.cos(), ang.sin());
        let half = len / 2;
        for i in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f32, 0.0f32);
            for k in 0..half {
                let (ur, ui) = (re[i + k], im[i + k]);
                let (vr, vi) = (
                    re[i + k + half] * cr - im[i + k + half] * ci,
                    re[i + k + half] * ci + im[i + k + half] * cr,
                );
                re[i + k] = ur + vr;
                im[i + k] = ui + vi;
                re[i + k + half] = ur - vr;
                im[i + k + half] = ui - vi;
                let ncr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = ncr;
            }
        }
        len <<= 1;
    }
}

fn hsl(h: f32, s: f32, l: f32) -> (u32, u32, u32) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = (h % 360.0 + 360.0) % 360.0 / 60.0;
    let x = c * (1.0 - ((hp % 2.0) - 1.0).abs());
    let (r, g, b) = match hp as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    (
        ((r + m) * 255.0) as u32,
        ((g + m) * 255.0) as u32,
        ((b + m) * 255.0) as u32,
    )
}

/// additive blend — canvas "lighter" composite
fn add(fb: &mut [u32], fw: usize, fh: usize, x: i32, y: i32,
       r: u32, g: u32, b: u32, a: u32) {
    if x < 0 || y < 0 || x >= fw as i32 || y >= fh as i32 || a == 0 {
        return;
    }
    let i = y as usize * fw + x as usize;
    let d = fb[i];
    let dr = ((d >> 16) & 0xff).min(255 - r * a / 255) + r * a / 255;
    let dg = ((d >> 8) & 0xff).min(255 - g * a / 255) + g * a / 255;
    let db = (d & 0xff).min(255 - b * a / 255) + b * a / 255;
    fb[i] = (dr << 16) | (dg << 8) | db;
}

fn disc(fb: &mut [u32], fw: usize, fh: usize, cx: i32, cy: i32,
        rad: i32, r: u32, g: u32, b: u32, a: u32) {
    for dy in -rad..=rad {
        for dx in -rad..=rad {
            if dx * dx + dy * dy <= rad * rad {
                add(fb, fw, fh, cx + dx, cy + dy, r, g, b, a);
            }
        }
    }
}

/// thick additive line — stamps discs along the segment; overdraw
/// builds up brightness the way the canvas "lighter" pass does
fn line(fb: &mut [u32], fw: usize, fh: usize,
        x0: f32, y0: f32, x1: f32, y1: f32, w: i32,
        r: u32, g: u32, b: u32, a: u32) {
    let dx = x1 - x0;
    let dy = y1 - y0;
    let steps = dx.abs().max(dy.abs()).max(1.0) as i32;
    let rad = (w / 2).max(1);
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        disc(fb, fw, fh,
             (x0 + dx * t) as i32, (y0 + dy * t) as i32,
             rad, r, g, b, a);
    }
}

/// fade the whole buffer toward near-black — motion trails (JS paints
/// rgba(4,4,12,0.30) instead of clearing)
pub fn trails(fb: &mut [u32]) {
    for p in fb.iter_mut() {
        let r = (*p >> 16) & 0xff;
        let g = (*p >> 8) & 0xff;
        let b = *p & 0xff;
        *p = ((r * 7 + 4 * 3) / 10) << 16
            | ((g * 7 + 4 * 3) / 10) << 8
            | ((b * 7 + 12 * 3) / 10);
    }
}

/// the full scene — same composition as the browser fallback:
/// spectrum spokes, counter-rotating wave rings, breathing core
pub fn draw(fb: &mut [u32], fw: usize, fh: usize,
            viz: &Viz, bands: &Bands) {
    trails(fb);
    let (cx, cy) = (fw as f32 / 2.0, fh as f32 / 2.0);
    let mind = fw.min(fh) as f32;
    let base_r = mind * 0.15 * (1.0 + viz.pulse * 0.45);
    let tau = std::f32::consts::TAU;

    // 96 radial spectrum spokes
    for (i, &v) in bands.spokes.iter().enumerate() {
        let ang = viz.phase + (i as f32 / SPOKES as f32) * tau;
        let len = 2.0 + v * v * mind * 0.30;
        let h = (viz.hue + i as f32 * (240.0 / SPOKES as f32)) % 360.0;
        let (r, g, b) = hsl(h, 0.9, 0.42 + v * 0.30);
        let a = (0.25 + v * 0.6) * 255.0;
        let w = (1.5 + v * 7.0) as i32;
        line(fb, fw, fh,
             cx + ang.cos() * base_r, cy + ang.sin() * base_r,
             cx + ang.cos() * (base_r + len),
             cy + ang.sin() * (base_r + len),
             w, r, g, b, a as u32);
    }

    // two counter-rotating waveform rings
    for ring in 0..2 {
        let dir = if ring == 1 { -1.0 } else { 1.0 };
        let r_base = base_r * (1.6 + ring as f32 * 0.55);
        let h = (viz.hue + ring as f32 * 120.0) % 360.0;
        let (r, g, b) = hsl(h, 0.85, 0.6);
        let mut px = 0f32;
        let mut py = 0f32;
        for i in 0..=RING_PTS {
            let s = bands.wave[i % RING_PTS];
            let ang = dir * viz.phase * 1.7
                + (i as f32 / RING_PTS as f32) * tau;
            let rr = r_base + s * mind * 0.09;
            let x = cx + ang.cos() * rr;
            let y = cy + ang.sin() * rr;
            if i > 0 {
                line(fb, fw, fh, px, py, x, y, 2, r, g, b, 128);
            }
            px = x;
            py = y;
        }
    }

    // breathing core — radial gradient disc, alpha falls off to the rim
    let (cr, cg, cb) = hsl(viz.hue, 0.9, 0.62);
    let ca = 0.35 + viz.pulse * 0.5;
    let ri = base_r as i32;
    for dy in -ri..=ri {
        for dx in -ri..=ri {
            let d2 = dx * dx + dy * dy;
            if d2 <= ri * ri {
                let fall = 1.0 - (d2 as f32).sqrt() / ri as f32;
                add(fb, fw, fh, cx as i32 + dx, cy as i32 + dy,
                    cr, cg, cb, (ca * fall * 255.0) as u32);
            }
        }
    }
}
