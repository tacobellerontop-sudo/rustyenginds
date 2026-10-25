//! rusty-wallpaper: animated desktop wallpapers rendered on the CPU.

mod app;
mod config;
mod ffmpeg;
mod platform;
mod render;
mod scene;
mod ui;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};

use config::Config;
use render::{Frame, FrameCtx};

#[derive(Parser)]
#[command(version, about, args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    run: RunArgs,
}

#[derive(Subcommand)]
enum Command {
    /// Show the wallpaper (the default when no command is given).
    Run(RunArgs),
    /// Open the settings window (also what double-clicking the exe does on Windows).
    Ui {
        /// Config file to edit [default: <config dir>/rusty-wallpaper/config.toml].
        #[arg(short, long)]
        config: Option<PathBuf>,
    },
    /// List the built-in scenes.
    List,
    /// List monitors and their indices.
    Monitors,
    /// Render a single frame of a scene to a PNG.
    Render(RenderArgs),
    /// Ask a running wallpaper to quit (Windows).
    Stop,
    /// Start the wallpaper automatically at login (Windows).
    Autostart {
        #[arg(value_parser = ["on", "off"])]
        state: String,
        /// Config file the autostarted engine should use.
        #[arg(short, long)]
        config: Option<PathBuf>,
    },
    /// Write an example config file to the default location.
    Init {
        /// Overwrite an existing config.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Args, Default)]
struct RunArgs {
    /// Scene to show, overriding the config.
    #[arg(short, long)]
    scene: Option<String>,
    /// Config file [default: <config dir>/rusty-wallpaper/config.toml].
    #[arg(short, long)]
    config: Option<PathBuf>,
    /// Frame-rate cap, overriding the config.
    #[arg(long)]
    fps: Option<u32>,
    /// Preview in a normal window instead of the desktop (Esc quits, Space pauses).
    #[arg(short, long)]
    windowed: bool,
    /// Detach from the console window (Windows; stop with `rusty-wallpaper stop`).
    #[arg(long, conflicts_with = "windowed")]
    background: bool,
}

#[derive(Args)]
struct RenderArgs {
    /// Output PNG path.
    out: PathBuf,
    #[arg(short, long, default_value = "aurora")]
    scene: String,
    /// Config file to read scene parameters from.
    #[arg(short, long)]
    config: Option<PathBuf>,
    #[arg(long, default_value_t = 1920)]
    width: usize,
    #[arg(long, default_value_t = 1080)]
    height: usize,
    /// Scene time in seconds to capture.
    #[arg(short, long, default_value_t = 5.0)]
    time: f32,
}

fn main() {
    let mut cli = Cli::parse();
    // Double-clicked from Explorer with no arguments: show the settings
    // window rather than a bare console.
    if cli.command.is_none() && std::env::args_os().len() == 1 && platform::launched_from_explorer() {
        platform::detach_console();
        cli.command = Some(Command::Ui { config: None });
    }
    let background = match &cli.command {
        None => cli.run.background,
        Some(Command::Run(a)) => a.background,
        _ => false,
    };
    init_logging(background);
    if let Err(e) = real_main(cli) {
        log::error!("{e:#}");
        std::process::exit(1);
    }
}

/// Log to stderr, or to a file when running detached from any console.
fn init_logging(background: bool) {
    let mut builder = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"));
    if background {
        let file = dirs::data_local_dir()
            .map(|d| d.join("rusty-wallpaper"))
            .and_then(|d| {
                std::fs::create_dir_all(&d)
                    .ok()
                    .map(|_| d.join("rusty-wallpaper.log"))
            })
            .and_then(|p| std::fs::File::create(p).ok());
        if let Some(file) = file {
            builder.target(env_logger::Target::Pipe(Box::new(file)));
        }
    }
    builder.init();
}

fn real_main(cli: Cli) -> Result<()> {
    match cli.command {
        None => run(cli.run),
        Some(Command::Run(args)) => run(args),
        Some(Command::List) => {
            for s in scene::SCENES {
                println!("{:<10} {}", s.name, s.description);
            }
            Ok(())
        }
        Some(Command::Monitors) => app::list_monitors(),
        Some(Command::Ui { config }) => {
            let path = config
                .or_else(Config::default_path)
                .context("couldn't determine the config directory")?;
            ui::run(path)
        }
        Some(Command::Render(args)) => render_png(args),
        Some(Command::Init { force }) => init(force),
        Some(Command::Stop) => {
            if platform::signal_stop()? {
                println!("asked the running wallpaper to stop");
            } else {
                println!("rusty-wallpaper is not running");
            }
            Ok(())
        }
        Some(Command::Autostart { state, config }) => {
            let enable = state == "on";
            let mut args = String::from("--background");
            if let Some(c) = config {
                let c = std::fs::canonicalize(&c).with_context(|| format!("config {}", c.display()))?;
                args.push_str(&format!(" --config \"{}\"", c.display()));
            }
            platform::autostart(enable, &args)?;
            println!("autostart {}", if enable { "enabled" } else { "disabled" });
            Ok(())
        }
    }
}

/// An explicit `--config` must exist; the default one is optional.
fn resolve_config(explicit: Option<PathBuf>) -> Result<Option<PathBuf>> {
    if let Some(p) = explicit {
        if !p.is_file() {
            bail!("config file {} not found", p.display());
        }
        return Ok(Some(p));
    }
    match Config::default_path() {
        Some(p) if p.is_file() => Ok(Some(p)),
        _ => {
            log::info!("no config file found, using defaults (run `rusty-wallpaper init` to create one)");
            Ok(None)
        }
    }
}

fn run(args: RunArgs) -> Result<()> {
    let config_path = resolve_config(args.config)?;
    if let Some(p) = &config_path {
        log::info!("using config {} (edits are applied live)", p.display());
    }
    if args.background {
        platform::detach_console();
    }
    app::run(app::Options {
        config_path,
        scene: args.scene,
        fps: args.fps,
        windowed: args.windowed,
    })
}

fn render_png(args: RenderArgs) -> Result<()> {
    let cfg = match resolve_config(args.config)? {
        Some(p) => Config::load(&p)?,
        None => Config::default(),
    };
    let mut scene = scene::build(&args.scene, &cfg.params_for(&args.scene))?;
    let mut frame = Frame::new(args.width, args.height);

    // Step up to the requested time so stateful scenes (starfield) evolve naturally.
    let dt = 1.0 / 30.0;
    let steps = (args.time / dt).ceil().max(1.0) as usize;
    let render_at = |scene: &mut Box<dyn scene::Scene>, frame: &mut Frame, i: usize| {
        let time = (i as f32 * dt).min(args.time);
        scene.render(
            frame,
            &FrameCtx {
                time,
                dt,
                cursor: None,
            },
        );
    };
    let warmup_start = steps.saturating_sub(90);
    for i in warmup_start..steps {
        render_at(&mut scene, &mut frame, i);
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    while !scene.is_ready() {
        if Instant::now() > deadline {
            bail!("scene did not finish loading in time");
        }
        std::thread::sleep(Duration::from_millis(20));
        render_at(&mut scene, &mut frame, steps);
    }
    render_at(&mut scene, &mut frame, steps);

    let rgb: Vec<u8> = frame
        .pixels
        .iter()
        .flat_map(|p| [(p >> 16) as u8, (p >> 8) as u8, *p as u8])
        .collect();
    image::RgbImage::from_raw(frame.width as u32, frame.height as u32, rgb)
        .context("building image")?
        .save(&args.out)
        .with_context(|| format!("writing {}", args.out.display()))?;
    println!("wrote {}", args.out.display());
    Ok(())
}

fn init(force: bool) -> Result<()> {
    let path = Config::default_path().context("couldn't determine the config directory")?;
    if path.exists() && !force {
        bail!("{} already exists (use --force to overwrite)", path.display());
    }
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(&path, config::EXAMPLE_CONFIG)?;
    println!("wrote {}", path.display());
    Ok(())
}
