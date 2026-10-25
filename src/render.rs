//! CPU frame buffer, shading helpers and the small math library scenes share.

use rayon::prelude::*;

/// Per-frame information handed to scenes.
#[derive(Clone, Copy, Debug)]
pub struct FrameCtx {
    /// Seconds since the engine started.
    pub time: f32,
    /// Seconds since the previous frame (clamped, never zero).
    pub dt: f32,
    /// Cursor position in normalized `0..1` coordinates, if known.
    pub cursor: Option<(f32, f32)>,
}

/// A 0RGB (`0x00RRGGBB`) pixel buffer, the format softbuffer presents.
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
}

impl Frame {
    pub fn new(width: usize, height: usize) -> Self {
        let (width, height) = (width.max(1), height.max(1));
        Self {
            width,
            height,
            pixels: vec![0; width * height],
        }
    }

    pub fn resize(&mut self, width: usize, height: usize) {
        let (width, height) = (width.max(1), height.max(1));
        if (width, height) != (self.width, self.height) {
            self.width = width;
            self.height = height;
            self.pixels = vec![0; width * height];
        }
    }

    pub fn aspect(&self) -> f32 {
        self.width as f32 / self.height as f32
    }

    /// Run `f(x, y)` for every pixel in parallel, like a fragment shader.
    /// Coordinates are pixel centres in `0..width`, `0..height`.
    pub fn par_shade<F>(&mut self, f: F)
    where
        F: Fn(f32, f32) -> u32 + Sync,
    {
        let w = self.width;
        self.pixels.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            let fy = y as f32 + 0.5;
            for (x, px) in row.iter_mut().enumerate() {
                *px = f(x as f32 + 0.5, fy);
            }
        });
    }

    /// Like [`Frame::par_shade`] but passes `uv` in `0..1` with `y` pointing up.
    pub fn par_shade_uv<F>(&mut self, f: F)
    where
        F: Fn(f32, f32) -> u32 + Sync,
    {
        let (iw, ih) = (1.0 / self.width as f32, 1.0 / self.height as f32);
        self.par_shade(|x, y| f(x * iw, 1.0 - y * ih));
    }

    /// Additively blend a soft round dot; used by particle scenes.
    pub fn splat(&mut self, cx: f32, cy: f32, radius: f32, color: [f32; 3]) {
        let r = radius.max(0.5);
        let x0 = (cx - r - 1.0).floor().max(0.0) as usize;
        let y0 = (cy - r - 1.0).floor().max(0.0) as usize;
        let x1 = ((cx + r + 1.0).ceil() as isize).min(self.width as isize);
        let y1 = ((cy + r + 1.0).ceil() as isize).min(self.height as isize);
        if x1 <= 0 || y1 <= 0 {
            return;
        }
        for y in y0..y1 as usize {
            for x in x0..x1 as usize {
                let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                let a = (1.0 - (d - r + 0.5)).clamp(0.0, 1.0);
                if a > 0.0 {
                    let idx = y * self.width + x;
                    let [br, bg, bb] = unpack(self.pixels[idx]);
                    self.pixels[idx] = pack(br + color[0] * a, bg + color[1] * a, bb + color[2] * a);
                }
            }
        }
    }

    /// Bilinearly scale this frame into `dst` (`dst_w * dst_h` pixels).
    pub fn upscale_into(&self, dst: &mut [u32], dst_w: usize, dst_h: usize) {
        if dst_w == self.width && dst_h == self.height {
            dst.copy_from_slice(&self.pixels);
            return;
        }
        let sx = self.width as f32 / dst_w as f32;
        let sy = self.height as f32 / dst_h as f32;
        let (sw, sh) = (self.width, self.height);
        let src = &self.pixels;
        dst.par_chunks_mut(dst_w).enumerate().for_each(|(y, row)| {
            let fy = ((y as f32 + 0.5) * sy - 0.5).clamp(0.0, (sh - 1) as f32);
            let y0 = fy as usize;
            let y1 = (y0 + 1).min(sh - 1);
            let ty = fy - y0 as f32;
            for (x, px) in row.iter_mut().enumerate() {
                let fx = ((x as f32 + 0.5) * sx - 0.5).clamp(0.0, (sw - 1) as f32);
                let x0 = fx as usize;
                let x1 = (x0 + 1).min(sw - 1);
                let tx = fx - x0 as f32;
                *px = lerp_px(
                    lerp_px(src[y0 * sw + x0], src[y0 * sw + x1], tx),
                    lerp_px(src[y1 * sw + x0], src[y1 * sw + x1], tx),
                    ty,
                );
            }
        });
    }
}

fn lerp_px(a: u32, b: u32, t: f32) -> u32 {
    let t = (t * 256.0) as u32;
    let it = 256 - t;
    let rb = (((a & 0xFF00FF) * it + (b & 0xFF00FF) * t) >> 8) & 0xFF00FF;
    let g = (((a & 0x00FF00) * it + (b & 0x00FF00) * t) >> 8) & 0x00FF00;
    rb | g
}

/// Pack linear-ish `0..1` floats into a 0RGB pixel.
#[inline]
pub fn pack(r: f32, g: f32, b: f32) -> u32 {
    let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
    (c(r) << 16) | (c(g) << 8) | c(b)
}

#[inline]
pub fn pack3(c: [f32; 3]) -> u32 {
    pack(c[0], c[1], c[2])
}

#[inline]
pub fn unpack(px: u32) -> [f32; 3] {
    [
        ((px >> 16) & 0xFF) as f32 / 255.0,
        ((px >> 8) & 0xFF) as f32 / 255.0,
        (px & 0xFF) as f32 / 255.0,
    ]
}

pub mod math {
    //! GLSL-flavoured helpers so scenes read like shaders.

    #[inline]
    pub fn mix(a: f32, b: f32, t: f32) -> f32 {
        a + (b - a) * t
    }

    #[inline]
    pub fn mix3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
        [mix(a[0], b[0], t), mix(a[1], b[1], t), mix(a[2], b[2], t)]
    }

    #[inline]
    pub fn add3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
        [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
    }

    #[inline]
    pub fn scale3(a: [f32; 3], s: f32) -> [f32; 3] {
        [a[0] * s, a[1] * s, a[2] * s]
    }

    #[inline]
    pub fn fract(x: f32) -> f32 {
        x - x.floor()
    }

    #[inline]
    pub fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
        let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }

    /// Cheap deterministic 2D -> 1D hash in `0..1`.
    #[inline]
    pub fn hash21(x: f32, y: f32) -> f32 {
        let mut px = fract(x * 123.34);
        let mut py = fract(y * 456.21);
        let d = px * (px + 45.32) + py * (py + 45.32);
        px += d;
        py += d;
        fract(px * py)
    }

    /// Integer hash in `0..1`, handy for seeding particles.
    #[inline]
    pub fn hash_u32(mut n: u32) -> f32 {
        n = (n ^ 61) ^ (n >> 16);
        n = n.wrapping_mul(9);
        n ^= n >> 4;
        n = n.wrapping_mul(0x27d4_eb2d);
        n ^= n >> 15;
        (n & 0x00FF_FFFF) as f32 / 0x0100_0000 as f32
    }

    /// Smooth 2D value noise in `0..1`.
    pub fn noise(x: f32, y: f32) -> f32 {
        let (ix, iy) = (x.floor(), y.floor());
        let (fx, fy) = (x - ix, y - iy);
        let ux = fx * fx * (3.0 - 2.0 * fx);
        let uy = fy * fy * (3.0 - 2.0 * fy);
        let a = hash21(ix, iy);
        let b = hash21(ix + 1.0, iy);
        let c = hash21(ix, iy + 1.0);
        let d = hash21(ix + 1.0, iy + 1.0);
        mix(mix(a, b, ux), mix(c, d, ux), uy)
    }

    /// Fractal Brownian motion built from [`noise`].
    pub fn fbm(mut x: f32, mut y: f32, octaves: u32) -> f32 {
        let (mut sum, mut amp, mut norm) = (0.0, 0.5, 0.0);
        for _ in 0..octaves {
            sum += amp * noise(x, y);
            norm += amp;
            // Rotate and scale between octaves to hide grid artifacts.
            let (nx, ny) = (x * 1.6 + y * 1.2, -x * 1.2 + y * 1.6);
            x = nx + 3.1;
            y = ny + 1.7;
            amp *= 0.5;
        }
        sum / norm
    }

    /// Inigo Quilez's cosine palette: `a + b * cos(2π(c·t + d))`.
    #[inline]
    pub fn palette(t: f32, a: [f32; 3], b: [f32; 3], c: [f32; 3], d: [f32; 3]) -> [f32; 3] {
        let tau = std::f32::consts::TAU;
        [
            a[0] + b[0] * (tau * (c[0] * t + d[0])).cos(),
            a[1] + b[1] * (tau * (c[1] * t + d[1])).cos(),
            a[2] + b[2] * (tau * (c[2] * t + d[2])).cos(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_roundtrip() {
        let px = pack(1.0, 0.5, 0.0);
        assert_eq!(px, 0xFF8000);
        let [r, g, b] = unpack(px);
        assert!((r - 1.0).abs() < 0.01 && (g - 0.5).abs() < 0.01 && b == 0.0);
    }

    #[test]
    fn upscale_preserves_solid_color() {
        let mut f = Frame::new(4, 3);
        f.pixels.fill(0x123456);
        let mut dst = vec![0; 17 * 9];
        f.upscale_into(&mut dst, 17, 9);
        assert!(dst.iter().all(|&p| p == 0x123456));
    }

    #[test]
    fn splat_is_clipped_at_edges() {
        let mut f = Frame::new(8, 8);
        f.splat(-3.0, -3.0, 5.0, [1.0, 1.0, 1.0]);
        f.splat(10.0, 10.0, 5.0, [1.0, 1.0, 1.0]);
        f.splat(100.0, 100.0, 5.0, [1.0, 1.0, 1.0]);
        assert!(f.pixels[0] != 0 && f.pixels[63] != 0);
    }

    #[test]
    fn noise_is_bounded() {
        for i in 0..1000 {
            let v = math::fbm(i as f32 * 0.37, i as f32 * -0.11, 5);
            assert!((0.0..=1.0).contains(&v));
        }
    }
}
