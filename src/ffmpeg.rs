//! Video decoding by piping raw frames out of an `ffmpeg` process.
//!
//! Shelling out keeps the build simple (no C libraries to link), plays every
//! format FFmpeg knows, and lets FFmpeg use hardware decoding where available.

use std::io::{BufRead, BufReader, ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender};

use anyhow::{bail, Context, Result};

use crate::platform;

/// File extensions treated as videos.
pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mkv", "webm", "mov", "avi", "m4v", "wmv", "gifv", "mpg", "mpeg",
];

pub fn is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| VIDEO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// `ffmpeg(.exe)` next to our own executable wins, then whatever is on PATH.
pub fn ffmpeg_path() -> PathBuf {
    let name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join(name)))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

fn command() -> Command {
    let mut cmd = Command::new(ffmpeg_path());
    cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
    platform::hide_console_window(&mut cmd);
    cmd
}

fn spawn(mut cmd: Command) -> Result<Child> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match cmd.spawn() {
        Ok(child) => Ok(child),
        Err(e) if e.kind() == ErrorKind::NotFound => bail!(
            "ffmpeg was not found. Put ffmpeg{} next to rusty-wallpaper or install it \
             (e.g. `winget install ffmpeg`)",
            std::env::consts::EXE_SUFFIX
        ),
        Err(e) => Err(e).context("starting ffmpeg"),
    }
}

/// How the video is fitted to the screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fit {
    /// Fill the screen, cropping the overflow.
    Cover,
    /// Show the whole video, letterboxed.
    Contain,
}

fn scale_filter(w: usize, h: usize, fit: Fit) -> String {
    match fit {
        Fit::Cover => format!("scale={w}:{h}:force_original_aspect_ratio=increase,crop={w}:{h}"),
        Fit::Contain => {
            format!("scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2")
        }
    }
}

/// A running decoder producing `w * h` 0RGB frames forever (the video loops).
pub struct Decoder {
    child: Child,
    frames: Receiver<Result<Vec<u32>>>,
}

impl Decoder {
    pub fn start(path: &Path, w: usize, h: usize, fps: f32, fit: Fit) -> Result<Self> {
        if !path.is_file() {
            bail!("video {} does not exist", path.display());
        }
        let mut cmd = command();
        if cfg!(windows) {
            // D3D11VA/DXVA2 decoding on the GPU; ffmpeg falls back to software.
            cmd.args(["-hwaccel", "auto"]);
        }
        cmd.args(["-stream_loop", "-1", "-i"])
            .arg(path)
            .args(["-an", "-sn", "-vf"])
            .arg(format!("fps={fps},{}", scale_filter(w, h, fit)))
            // bgr0 is B,G,R,0 in memory: exactly a little-endian 0x00RRGGBB u32.
            .args(["-f", "rawvideo", "-pix_fmt", "bgr0", "-"]);
        let mut child = spawn(cmd)?;
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");

        // A tiny queue: when we stop pulling frames (paused, covered), ffmpeg
        // blocks on the pipe and stops using CPU.
        let (tx, rx) = mpsc::sync_channel(2);
        let err_tx = tx.clone();
        std::thread::spawn(move || read_frames(stdout, w * h, tx));
        std::thread::spawn(move || forward_errors(stderr, err_tx));
        Ok(Self { child, frames: rx })
    }

    /// The next decoded frame, if one is ready. `Err` means the decoder died.
    pub fn try_next(&self) -> Option<Result<Vec<u32>>> {
        match self.frames.try_recv() {
            Ok(r) => Some(r),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Err(anyhow::anyhow!("ffmpeg exited"))),
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn read_frames(mut out: impl Read, pixels: usize, tx: SyncSender<Result<Vec<u32>>>) {
    let mut bytes = vec![0u8; pixels * 4];
    loop {
        if let Err(e) = out.read_exact(&mut bytes) {
            if e.kind() != ErrorKind::UnexpectedEof {
                let _ = tx.send(Err(anyhow::anyhow!("reading video frames: {e}")));
            }
            return;
        }
        let frame = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&[b, g, r, _]| u32::from_le_bytes([b, g, r, 0]))
            .collect();
        if tx.send(Ok(frame)).is_err() {
            return; // decoder dropped
        }
    }
}

/// ffmpeg only prints to stderr when something is wrong (`-loglevel error`).
fn forward_errors(err: impl Read, tx: SyncSender<Result<Vec<u32>>>) {
    let mut lines = Vec::new();
    for line in BufReader::new(err).lines().map_while(Result::ok) {
        log::warn!("ffmpeg: {line}");
        lines.push(line);
    }
    if !lines.is_empty() {
        // Usually a fatal error (bad file, unsupported codec); surface the last line.
        let _ = tx.send(Err(anyhow::anyhow!("ffmpeg: {}", lines.last().unwrap())));
    }
}

/// Grab one RGBA frame, cropped to `w x h`, for a gallery thumbnail.
pub fn thumbnail(path: &Path, w: usize, h: usize) -> Result<Vec<u8>> {
    // A couple of seconds in usually skips black intros; very short clips
    // fall back to the first frame.
    for seek in ["2", "0"] {
        let mut cmd = command();
        cmd.args(["-ss", seek, "-i"])
            .arg(path)
            .args(["-frames:v", "1", "-an", "-vf"])
            .arg(scale_filter(w, h, Fit::Cover))
            .args(["-f", "rawvideo", "-pix_fmt", "rgba", "-"]);
        let out = cmd.stdin(Stdio::null()).stderr(Stdio::null()).output();
        let out = match out {
            Err(e) if e.kind() == ErrorKind::NotFound => bail!("ffmpeg was not found"),
            other => other.context("running ffmpeg")?,
        };
        if out.stdout.len() == w * h * 4 {
            return Ok(out.stdout);
        }
    }
    bail!("couldn't read a frame from {}", path.display())
}
