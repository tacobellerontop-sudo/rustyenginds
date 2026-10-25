use crate::render::math::*;
use crate::render::{pack3, Frame, FrameCtx};
use crate::scene::{Params, Scene};

const HORIZON: f32 = 0.36;

pub struct Synthwave {
    speed: f32,
    sun: [f32; 3],
    grid: [f32; 3],
}

impl Synthwave {
    pub fn new(p: &Params) -> Self {
        Self {
            speed: p.f32("speed", 1.0),
            sun: p.color("sun", [1.0, 0.16, 0.43]),
            grid: p.color("grid", [0.02, 0.85, 0.91]),
        }
    }
}

impl Scene for Synthwave {
    fn render(&mut self, frame: &mut Frame, ctx: &FrameCtx) {
        let t = ctx.time * self.speed;
        let aspect = frame.aspect();
        let pixel = 1.0 / frame.height as f32;
        let (sun_col, grid_col) = (self.sun, self.grid);
        let haze = [0.95, 0.35, 0.55];

        frame.par_shade_uv(|u, v| {
            let x = (u - 0.5) * aspect;
            if v >= HORIZON {
                // Sky.
                let h = (v - HORIZON) / (1.0 - HORIZON);
                let mut c = mix3(haze, [0.08, 0.02, 0.18], smoothstep(0.0, 0.7, h).sqrt());

                // Sun with the classic horizontal cut-outs on its lower half.
                let (sx, sy, r) = (0.0, HORIZON + 0.2, 0.2);
                let d = ((x - sx).powi(2) + (v - sy).powi(2)).sqrt();
                let sv = (v - (sy - r)) / (2.0 * r); // 0 at bottom, 1 at top
                let stripes = if sv < 0.55 {
                    let band = fract(sv * 11.0 + t * 0.15);
                    let gap = (0.55 - sv) * 0.9;
                    smoothstep(gap - 0.04, gap + 0.04, band)
                } else {
                    1.0
                };
                let disc = (1.0 - smoothstep(r - pixel * 1.5, r, d)) * stripes;
                let body = mix3(sun_col, [1.0, 0.85, 0.3], smoothstep(0.1, 1.0, sv));
                let glow = (-(d - r).max(0.0) * 9.0).exp() * 0.45;
                c = add3(c, scale3(sun_col, glow));
                c = mix3(c, body, disc);
                return pack3(c);
            }

            // Ground: perspective grid scrolling towards the viewer.
            let below = HORIZON - v; // > 0
            let z = 0.12 / below; // depth
            let wx = x * z * 6.0;
            let wz = z * 2.0 + t * 1.2;
            // Approximate one pixel's footprint in world units for anti-aliasing.
            let fz = z * z / 0.12 * 2.0 * pixel * 1.5;
            let fx = (z * 6.0 * pixel * 1.5).max(1e-4);
            let near_int = |a: f32| (fract(a + 0.5) - 0.5).abs();
            let lz = 1.0 - smoothstep(0.03, 0.03 + fz, near_int(wz));
            let lx = 1.0 - smoothstep(0.03, 0.03 + fx, near_int(wx));
            // Lines narrower than a pixel average out instead of aliasing.
            let line = lz.max(lx) * (0.04 / (0.04 + fz)).min(1.0);
            let fog = (-below * 7.0).exp();
            let mut c = mix3([0.03, 0.0, 0.07], [0.1, 0.0, 0.15], fog);
            c = add3(c, scale3(grid_col, line * (1.0 - fog * 0.5)));
            c = mix3(c, haze, fog * 0.6);
            pack3(c)
        });
    }
}
