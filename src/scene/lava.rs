use std::f32::consts::TAU;

use crate::render::math::*;
use crate::render::{pack3, Frame, FrameCtx};
use crate::scene::{Params, Scene};

struct Blob {
    radius: f32,
    freq: (f32, f32),
    phase: (f32, f32),
}

pub struct Lava {
    speed: f32,
    color_a: [f32; 3],
    color_b: [f32; 3],
    background: [f32; 3],
    blobs: Vec<Blob>,
    positions: Vec<(f32, f32, f32)>,
}

impl Lava {
    pub fn new(p: &Params) -> Self {
        let n = p.usize("blobs", 7, 64).max(1);
        let blobs = (0..n as u32)
            .map(|i| Blob {
                radius: 0.06 + 0.07 * hash_u32(i * 7 + 1),
                freq: (
                    0.05 + 0.08 * hash_u32(i * 7 + 2),
                    0.04 + 0.07 * hash_u32(i * 7 + 3),
                ),
                phase: (hash_u32(i * 7 + 4) * TAU, hash_u32(i * 7 + 5) * TAU),
            })
            .collect();
        Self {
            speed: p.f32("speed", 1.0),
            color_a: p.color("color_a", [1.0, 0.3, 0.0]),
            color_b: p.color("color_b", [1.0, 0.82, 0.0]),
            background: p.color("background", [0.1, 0.024, 0.2]),
            blobs,
            positions: Vec::new(),
        }
    }
}

impl Scene for Lava {
    fn render(&mut self, frame: &mut Frame, ctx: &FrameCtx) {
        let t = ctx.time * self.speed;
        let aspect = frame.aspect();

        self.positions.clear();
        for b in &self.blobs {
            let x = aspect * (0.5 + 0.42 * (t * b.freq.0 * TAU + b.phase.0).sin());
            let y = 0.5 + 0.45 * (t * b.freq.1 * TAU + b.phase.1).sin();
            self.positions.push((x, y, b.radius * b.radius));
        }
        // The cursor (when we get one) drags an extra blob around.
        if let Some((cx, cy)) = ctx.cursor {
            self.positions.push((cx * aspect, 1.0 - cy, 0.006));
        }

        let positions = &self.positions;
        let (ca, cb, bg) = (self.color_a, self.color_b, self.background);
        frame.par_shade_uv(|u, v| {
            let x = u * aspect;
            let mut field = 0.0;
            for &(bx, by, r2) in positions {
                let d2 = (x - bx) * (x - bx) + (v - by) * (v - by);
                field += r2 / (d2 + 1e-4);
            }
            // Vertical background gradient with a warm glow around the blobs.
            let mut c = scale3(bg, 0.6 + 0.4 * v);
            c = add3(c, scale3(ca, smoothstep(0.25, 1.0, field) * 0.25));
            let inside = smoothstep(0.92, 1.05, field);
            let hot = mix3(ca, cb, smoothstep(1.0, 3.5, field));
            pack3(mix3(c, hot, inside))
        });
    }
}
