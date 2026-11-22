mod config;
mod smart_tap;
mod touch_scroll;
mod vlc;
mod x11_helper;

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "vlc-gestures",
    author = "Nandha <myselfnandha@gmail.com>",
    version = "2.0.0",
    about = "Unified Linux Input & Gesture Daemon (VLC Hold-Speed, Touch Scroll, Smart Tap)"
)]
struct Cli {
    /// Custom path to configuration file (default: ~/.config/vlc-gestures/config.toml)
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Run only the VLC gesture engine
    #[arg(long)]
    vlc_only: bool,

    /// Run only the Touchscreen scroll engine
    #[arg(long)]
    touch_only: bool,

    /// Run only the Smart 3-Finger Tap engine
    #[arg(long)]
    tap_only: bool,

    /// Print default configuration to stdout and exit
    #[arg(long)]
    print_default_config: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Cli::parse();

    if args.print_default_config {
        let default_cfg = config::Config::default();
        println!("{}", toml::to_string_pretty(&default_cfg)?);
        return Ok(());
    }

    let config_path = args.config.unwrap_or_else(config::Config::default_path);
    let mut cfg = config::Config::load_or_create(&config_path);

    // Apply CLI overrides
    if args.vlc_only {
        cfg.vlc.enabled = true;
        cfg.touch_scroll.enabled = false;
        cfg.smart_tap.enabled = false;
    } else if args.touch_only {
        cfg.vlc.enabled = false;
        cfg.touch_scroll.enabled = true;
        cfg.smart_tap.enabled = false;
    } else if args.tap_only {
        cfg.vlc.enabled = false;
        cfg.touch_scroll.enabled = false;
        cfg.smart_tap.enabled = true;
    }

    // Initialize structured logging
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&cfg.general.log_level));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    info!("=======================================================");
    info!("🚀 Starting vlc-gestures v2.0.0 (Unified Rust Daemon)");
    info!("=======================================================");
    info!("Config: {:?}", config_path);

    // Connect to X11 Display Server
    let x11 = match x11_helper::X11Context::new() {
        Ok(ctx) => Arc::new(ctx),
        Err(e) => {
            error!("Failed to initialize X11 context: {e}");
            return Err(e);
        }
    };

    let mut handles = Vec::new();

    // 1. VLC Gesture Engine
    if cfg.vlc.enabled {
        let x11_clone = Arc::clone(&x11);
        let vlc_cfg = cfg.vlc.clone();
        handles.push(tokio::spawn(async move {
            if let Err(e) = vlc::run_vlc_monitor(x11_clone, vlc_cfg).await {
                error!("VLC gesture worker encountered error: {e}");
            }
        }));
    }

    // 2. Touch Scroll Engine
    if cfg.touch_scroll.enabled {
        let x11_clone = Arc::clone(&x11);
        let ts_cfg = cfg.touch_scroll.clone();
        handles.push(tokio::spawn(async move {
            if let Err(e) = touch_scroll::run_touch_scroll(x11_clone, ts_cfg).await {
                error!("Touch scroll worker encountered error: {e}");
            }
        }));
    }

    // 3. Smart 3-Finger Tap Engine
    if cfg.smart_tap.enabled {
        let x11_clone = Arc::clone(&x11);
        let tap_cfg = cfg.smart_tap.clone();
        handles.push(tokio::spawn(async move {
            if let Err(e) = smart_tap::run_smart_tap(x11_clone, tap_cfg).await {
                error!("Smart tap worker encountered error: {e}");
            }
        }));
    }

    info!("All active gesture modules initialized and running.");

    // Wait for shutdown signal (Ctrl+C or SIGTERM)
    tokio::signal::ctrl_c().await?;
    info!("Termination signal received. Cleaning up...");

    // Restore touchpad button map
    if cfg.smart_tap.enabled {
        smart_tap::configure_touchpad_map(&cfg.smart_tap.device_name_pattern, 2, false);
    }

    for h in handles {
        h.abort();
    }

    info!("vlc-gestures daemon stopped cleanly.");
    Ok(())
}
