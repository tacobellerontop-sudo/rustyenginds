use crate::render::math::*;
use crate::render::{pack3, Frame, FrameCtx};
use crate::scene::{Params, Scene};

pub struct Plasma {
    speed: f32,
    scale: f32,
    shift: f32,
}

impl Plasma {
    pub fn new(p: &Params) -> Self {
        Self {
            speed: p.f32("speed", 1.0),
            scale: p.f32("scale", 8.0),
            shift: p.f32("palette_shift", 0.0),
        }
    }
}

impl Scene for Plasma {
    fn render(&mut self, frame: &mut Frame, ctx: &FrameCtx) {
        let t = ctx.time * self.speed;
        let (s, shift, aspect) = (self.scale, self.shift, frame.aspect());
        let (cx_off, cy_off) = ((t * 0.2).sin() * 0.5 * s, (t * 0.33).cos() * 0.5 * s);
        frame.par_shade_uv(|u, v| {
            let x = (u - 0.5) * aspect * s;
            let y = (v - 0.5) * s;
            let mut val = (x + t).sin();
            val += ((y + t) * 0.5).sin();
            val += ((x + y + t) * 0.5).sin();
            let (cx, cy) = (x + cx_off, y + cy_off);
            val += ((cx * cx + cy * cy + 1.0).sqrt() + t).sin();
            let c = palette(
                val * 0.25 + shift + t * 0.03,
                [0.5, 0.5, 0.5],
                [0.5, 0.5, 0.5],
                [1.0, 1.0, 1.0],
                [0.0, 0.33, 0.67],
            );
            pack3(c)
        });
    }
}
