use crate::render::math::*;
use crate::render::{pack3, Frame, FrameCtx};
use crate::scene::{Params, Scene};

const LAYERS: usize = 3;

pub struct Aurora {
    speed: f32,
    intensity: f32,
    color_a: [f32; 3],
    color_b: [f32; 3],
    stars: f32,
    /// Per-column curtain baseline and ray brightness for each layer.
    /// Both depend only on x, so computing them once per column keeps the
    /// per-pixel work tiny.
    columns: Vec<[(f32, f32); LAYERS]>,
    /// Mountain ridge height per column (static, recomputed on resize).
    ridge: Vec<f32>,
}

impl Aurora {
    pub fn new(p: &Params) -> Self {
        Self {
            speed: p.f32("speed", 1.0),
            intensity: p.f32("intensity", 1.0),
            color_a: p.color("color_a", [0.13, 1.0, 0.6]),
            color_b: p.color("color_b", [0.48, 0.24, 1.0]),
            stars: p.f32("stars", 1.0),
            columns: Vec::new(),
            ridge: Vec::new(),
        }
    }
}

impl Scene for Aurora {
    fn render(&mut self, frame: &mut Frame, ctx: &FrameCtx) {
        let t = ctx.time * self.speed;
        let aspect = frame.aspect();
        let w = frame.width;

        if self.ridge.len() != w {
            self.ridge = (0..w)
                .map(|col| {
                    let x = (col as f32 + 0.5) / w as f32 * aspect;
                    0.13 + 0.07 * fbm(x * 2.2 + 11.0, 4.0, 4) + 0.03 * (x * 7.0).sin().abs()
                })
                .collect();
        }
        self.columns.resize(w, [(0.0, 0.0); LAYERS]);
        for (col, out) in self.columns.iter_mut().enumerate() {
            let x = (col as f32 + 0.5) / w as f32 * aspect;
            for (i, slot) in out.iter_mut().enumerate() {
                let fi = i as f32;
                let base = 0.46
                    + fi * 0.08
                    + 0.10 * (x * (0.9 + fi * 0.35) + t * 0.07 + fi * 2.1).sin()
                    + 0.12 * (fbm(x * 1.3 + t * 0.03, fi * 7.3 + t * 0.02, 3) - 0.5);
                let rays = fbm(x * (9.0 + fi * 3.0) + t * 0.12, fi * 13.1 + t * 0.05, 3);
                let rays = smoothstep(0.25, 0.8, rays);
                // Slow breathing so the curtains brighten and fade.
                let pulse = 0.65 + 0.35 * (t * 0.21 + fi * 1.7 + x * 0.6).sin();
                *slot = (base, rays * pulse);
            }
        }

        let columns = &self.columns;
        let ridges = &self.ridge;
        let (ca, cb) = (self.color_a, self.color_b);
        let intensity = self.intensity;
        let star_amt = self.stars;
        let (fw, fh) = (frame.width as f32, frame.height as f32);

        frame.par_shade(|px, py| {
            let (u, v) = (px / fw, 1.0 - py / fh);
            let x = u * aspect;

            // Night sky gradient.
            let mut c = mix3([0.02, 0.05, 0.10], [0.004, 0.008, 0.03], smoothstep(0.0, 1.0, v));

            // Twinkling stars on a fixed grid (resolution independent).
            if star_amt > 0.0 {
                let gx = (x * 220.0).floor();
                let gy = (v * 220.0).floor();
                let h = hash21(gx, gy);
                if h > 0.993 {
                    let tw = 0.55 + 0.45 * (ctx.time * (1.0 + h * 4.0) + h * 91.0).sin();
                    let s = (h - 0.993) / 0.007 * tw * star_amt * smoothstep(0.15, 0.5, v);
                    c = add3(c, [s * 0.9, s * 0.9, s]);
                }
            }

            // Aurora curtains: sharp lower edge, long soft fade upward.
            let ci = (px as usize).min(columns.len() - 1);
            let col = &columns[ci];
            for (base, rays) in col {
                let d = v - base;
                if d > -0.03 {
                    let lower = smoothstep(-0.03, 0.0, d);
                    let upper = (-d.max(0.0) * 3.2).exp();
                    let glow = lower * upper * rays * 0.55 * intensity;
                    let tint = mix3(ca, cb, smoothstep(0.0, 0.3, d));
                    c = add3(c, scale3(tint, glow));
                }
            }

            // Mountain silhouette with a faint rim of aurora light.
            let ridge = ridges[ci];
            if v < ridge {
                let rim = smoothstep(ridge - 0.012, ridge, v) * 0.12;
                c = add3([0.004, 0.008, 0.016], scale3(ca, rim * intensity));
            }

            pack3(c)
        });
    }
}
