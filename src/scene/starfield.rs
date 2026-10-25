use crate::render::math::*;
use crate::render::{Frame, FrameCtx};
use crate::scene::{Params, Scene};

const NEAR: f32 = 0.02;

struct Star {
    x: f32,
    y: f32,
    z: f32,
    tint: [f32; 3],
}

pub struct Starfield {
    speed: f32,
    warp: f32,
    stars: Vec<Star>,
    spawned: u32,
    /// Static nebula backdrop, cached per resolution.
    background: Vec<u32>,
    bg_size: (usize, usize),
}

impl Starfield {
    pub fn new(p: &Params) -> Self {
        let count = p.usize("count", 900, 20_000);
        let mut s = Self {
            speed: p.f32("speed", 1.0),
            warp: p.f32("warp", 0.0).max(0.0),
            stars: Vec::with_capacity(count),
            spawned: 0,
            background: Vec::new(),
            bg_size: (0, 0),
        };
        for _ in 0..count {
            let mut star = s.spawn();
            // Spread the initial stars through the whole depth range.
            star.z = NEAR + hash_u32(s.spawned * 5 + 3) * (1.0 - NEAR);
            s.stars.push(star);
        }
        s
    }

    fn spawn(&mut self) -> Star {
        self.spawned = self.spawned.wrapping_add(1);
        let n = self.spawned * 5;
        let warm = hash_u32(n + 4);
        Star {
            x: hash_u32(n) * 2.0 - 1.0,
            y: hash_u32(n + 1) * 2.0 - 1.0,
            z: 1.0,
            tint: mix3([0.7, 0.8, 1.0], [1.0, 0.85, 0.7], warm),
        }
    }

    fn ensure_background(&mut self, frame: &Frame) {
        if self.bg_size == (frame.width, frame.height) {
            return;
        }
        let aspect = frame.aspect();
        let mut bg = Frame::new(frame.width, frame.height);
        bg.par_shade_uv(|u, v| {
            let (x, y) = (u * aspect * 2.0, v * 2.0);
            let n = fbm(x + 5.0, y, 5);
            let m = fbm(x * 1.7 - 3.0, y * 1.7 + 8.0, 4);
            let neb = smoothstep(0.4, 0.8, n) * 0.3;
            let c = add3(
                [0.005, 0.006, 0.016],
                add3(
                    scale3([0.35, 0.1, 0.6], neb),
                    scale3([0.05, 0.25, 0.45], smoothstep(0.45, 0.85, m) * 0.2),
                ),
            );
            crate::render::pack3(c)
        });
        self.background = bg.pixels;
        self.bg_size = (frame.width, frame.height);
    }
}

impl Scene for Starfield {
    fn render(&mut self, frame: &mut Frame, ctx: &FrameCtx) {
        self.ensure_background(frame);
        frame.pixels.copy_from_slice(&self.background);

        let (w, h) = (frame.width as f32, frame.height as f32);
        let (cx, cy) = (w * 0.5, h * 0.5);
        let focal = w.max(h) * 0.5;
        let dz = ctx.dt * self.speed * 0.25;
        let res = h / 540.0; // keep star sizes consistent across render scales
        let streak = 1.0 + self.warp * 12.0;

        for i in 0..self.stars.len() {
            let star = &mut self.stars[i];
            let old_z = star.z;
            star.z -= dz;
            let project = |z: f32| (cx + star.x / z * focal, cy + star.y / z * focal);
            let (sx, sy) = project(star.z.max(NEAR));
            let offscreen = sx < -4.0 || sy < -4.0 || sx > w + 4.0 || sy > h + 4.0;
            if star.z <= NEAR || offscreen {
                self.stars[i] = self.spawn();
                continue;
            }

            let depth = 1.0 - star.z; // 0 far .. 1 near
            let fade_in = smoothstep(0.0, 0.15, depth);
            let brightness = (0.15 + depth * depth * 1.2) * fade_in;
            let radius = (0.35 + depth * depth * 2.2) * res;
            let color = scale3(star.tint, brightness);

            // Streak from where the star "was" (stretched by warp) to where it is.
            let tail_z = (old_z + dz * (streak - 1.0)).min(1.0);
            let (tx, ty) = project(tail_z);
            let len = ((sx - tx).powi(2) + (sy - ty).powi(2)).sqrt();
            let steps = (len / radius.max(0.5)).ceil().clamp(1.0, 64.0) as usize;
            let per = 1.0 / steps as f32;
            for s in 0..steps {
                let k = s as f32 * per;
                let a = if steps == 1 {
                    1.0
                } else {
                    (1.0 - k).powf(0.5) * per * 2.0
                };
                frame.splat(mix(sx, tx, k), mix(sy, ty, k), radius, scale3(color, a.min(1.0)));
            }
        }
    }
}
