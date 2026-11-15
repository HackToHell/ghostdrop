//! Minimal software drawing into the window's BGRA buffer: rectangles, text
//! (fontdue + the system UI font from fontconfig), and terminal frames.
use crate::tab::Frame;
use std::collections::HashMap;

pub type Rgb = [u8; 3];

pub struct Canvas<'a> {
    pub buf: &'a mut [u8],
    pub width: usize,
    pub height: usize,
    pub stride: usize,
}

fn px(c: Rgb, a: u8) -> [u8; 4] {
    [c[2], c[1], c[0], a]
}

impl<'a> Canvas<'a> {
    pub fn fill(&mut self, x: i32, y: i32, w: i32, h: i32, c: Rgb) {
        let (x0, y0) = (x.max(0) as usize, y.max(0) as usize);
        let x1 = ((x + w).max(0) as usize).min(self.width);
        let y1 = ((y + h).max(0) as usize).min(self.height);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let p = px(c, 255);
        for yy in y0..y1 {
            let row = &mut self.buf[yy * self.stride + x0 * 4..yy * self.stride + x1 * 4];
            for chunk in row.chunks_exact_mut(4) {
                chunk.copy_from_slice(&p);
            }
        }
    }

    /// Alpha-blend a coverage mask (0..255) of color `c` at (x, y).
    fn blend_mask(&mut self, x: i32, y: i32, w: usize, h: usize, mask: &[u8], c: Rgb, clip_x1: i32) {
        for my in 0..h {
            let yy = y + my as i32;
            if yy < 0 || yy as usize >= self.height {
                continue;
            }
            for mx in 0..w {
                let xx = x + mx as i32;
                if xx < 0 || xx as usize >= self.width || xx >= clip_x1 {
                    continue;
                }
                let a = mask[my * w + mx] as u32;
                if a == 0 {
                    continue;
                }
                let i = yy as usize * self.stride + xx as usize * 4;
                let d = &mut self.buf[i..i + 4];
                for (k, ch) in [c[2], c[1], c[0]].iter().enumerate() {
                    d[k] = ((*ch as u32 * a + d[k] as u32 * (255 - a)) / 255) as u8;
                }
                d[3] = 255;
            }
        }
    }

    /// Copy a terminal frame into the rectangle (x, y, w, h); uncovered space
    /// is filled with `bg`.
    pub fn blit_frame(&mut self, frame: Option<&Frame>, x: usize, y: usize, w: usize, h: usize, bg: Rgb) {
        let w = w.min(self.width.saturating_sub(x));
        let h = h.min(self.height.saturating_sub(y));
        let (fw, fh) = frame.map_or((0, 0), |f| (f.width as usize, f.height as usize));
        let cw = fw.min(w);
        let ch = fh.min(h);
        if let Some(f) = frame {
            for row in 0..ch {
                let src = &f.row(row as u32)[..cw * 4];
                let o = (y + row) * self.stride + x * 4;
                self.buf[o..o + cw * 4].copy_from_slice(src);
            }
        }
        if cw < w {
            self.fill((x + cw) as i32, y as i32, (w - cw) as i32, ch as i32, bg);
        }
        if ch < h {
            self.fill(x as i32, (y + ch) as i32, w as i32, (h - ch) as i32, bg);
        }
    }
}

pub fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8;
    [m(a[0], b[0]), m(a[1], b[1]), m(a[2], b[2])]
}

pub fn luminance(c: Rgb) -> f32 {
    (0.299 * c[0] as f32 + 0.587 * c[1] as f32 + 0.114 * c[2] as f32) / 255.0
}

/// UI font (tab titles, prompts). Glyphs are rasterized once and cached.
pub struct Font {
    font: Option<fontdue::Font>,
    cache: HashMap<(char, u32), (fontdue::Metrics, Vec<u8>)>,
}

fn find_ui_font() -> Option<std::path::PathBuf> {
    for pattern in ["sans-serif:weight=medium", "sans-serif"] {
        if let Ok(out) = std::process::Command::new("fc-match").args(["-f", "%{file}", pattern]).output() {
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !p.is_empty() && std::path::Path::new(&p).exists() {
                return Some(p.into());
            }
        }
    }
    [
        "/usr/share/fonts/noto/NotoSans-Medium.ttf",
        "/usr/share/fonts/noto/NotoSans-Regular.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans.ttf",
    ]
    .iter()
    .map(std::path::PathBuf::from)
    .find(|p| p.exists())
}

impl Font {
    pub fn load() -> Font {
        let font = find_ui_font()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|bytes| fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default()).ok());
        if font.is_none() {
            eprintln!("ghostdrop: no UI font found (tab titles won't be drawn)");
        }
        Font { font, cache: HashMap::new() }
    }

    fn glyph(&mut self, c: char, size: f32) -> Option<&(fontdue::Metrics, Vec<u8>)> {
        let font = self.font.as_ref()?;
        let key = (c, (size * 4.0) as u32);
        Some(self.cache.entry(key).or_insert_with(|| font.rasterize(c, size)))
    }

    pub fn measure(&mut self, text: &str, size: f32) -> f32 {
        text.chars().map(|c| self.glyph(c, size).map_or(0.0, |g| g.0.advance_width)).sum()
    }

    pub fn line_metrics(&self, size: f32) -> (f32, f32) {
        self.font
            .as_ref()
            .and_then(|f| f.horizontal_line_metrics(size))
            .map_or((size * 0.8, size * 0.2), |m| (m.ascent, -m.descent))
    }

    /// Truncate with an ellipsis to fit `max_w`.
    pub fn fit(&mut self, text: &str, size: f32, max_w: f32) -> String {
        if self.measure(text, size) <= max_w {
            return text.to_string();
        }
        let ell = self.measure("…", size);
        let mut out = String::new();
        let mut w = 0.0;
        for c in text.chars() {
            let cw = self.glyph(c, size).map_or(0.0, |g| g.0.advance_width);
            if w + cw + ell > max_w {
                break;
            }
            w += cw;
            out.push(c);
        }
        out.push('…');
        out
    }

    /// Like `fit`, but keeps the end of the text (for input fields).
    pub fn fit_tail(&mut self, text: &str, size: f32, max_w: f32) -> String {
        if self.measure(text, size) <= max_w {
            return text.to_string();
        }
        let ell = self.measure("…", size);
        let mut tail: Vec<char> = Vec::new();
        let mut w = 0.0;
        for c in text.chars().rev() {
            let cw = self.glyph(c, size).map_or(0.0, |g| g.0.advance_width);
            if w + cw + ell > max_w {
                break;
            }
            w += cw;
            tail.push(c);
        }
        std::iter::once('…').chain(tail.into_iter().rev()).collect()
    }

    /// Draw `text` with its baseline at `baseline`; returns the advance.
    pub fn draw(&mut self, canvas: &mut Canvas, x: f32, baseline: f32, text: &str, size: f32, color: Rgb, clip_x1: i32) -> f32 {
        let mut pen = x;
        for c in text.chars() {
            let Some(g) = self.glyph(c, size) else { continue };
            let m = g.0;
            let gx = (pen + m.xmin as f32).round() as i32;
            let gy = (baseline - m.height as f32 - m.ymin as f32).round() as i32;
            canvas.blend_mask(gx, gy, m.width, m.height, &g.1, color, clip_x1);
            pen += m.advance_width;
        }
        pen - x
    }
}
