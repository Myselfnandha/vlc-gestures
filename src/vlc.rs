use anyhow::Result;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, info};
use zbus::proxy;

use crate::config::VlcConfig;
use crate::x11_helper::{PointerInfo, WindowGeometry, X11Context};

#[proxy(
    interface = "org.mpris.MediaPlayer2.Player",
    default_service = "org.mpris.MediaPlayer2.vlc",
    default_path = "/org/mpris/MediaPlayer2"
)]
pub trait MprisPlayer {
    fn play_pause(&self) -> zbus::Result<()>;
    #[zbus(property)]
    fn rate(&self) -> zbus::Result<f64>;
    #[zbus(property)]
    fn set_rate(&self, rate: f64) -> zbus::Result<()>;
    #[zbus(property)]
    fn playback_status(&self) -> zbus::Result<String>;
}

pub async fn run_vlc_monitor(x11: Arc<X11Context>, config: VlcConfig) -> Result<()> {
    if !config.enabled {
        info!("VLC gesture monitor disabled in configuration");
        return Ok(());
    }

    info!(
        "Starting VLC gesture monitor (Hold: {}ms, Rate: {}x)",
        config.hold_threshold_ms, config.accelerated_rate
    );

    let dbus_conn = match zbus::connection::Connection::session().await {
        Ok(c) => Some(c),
        Err(e) => {
            debug!("D-Bus session unavailable initially: {e}");
            None
        }
    };

    let mut is_pressed = false;
    let mut press_start: Option<Instant> = None;
    let mut accelerated = false;
    let mut original_rate = 1.0;
    let mut pending_click = false;
    let mut pending_click_time: Option<Instant> = None;

    let mut last_active_id = None;
    let mut is_vlc = false;
    let mut cached_geo: Option<WindowGeometry> = None;
    let mut is_fullscreen = false;
    let mut last_geo_check = Instant::now() - Duration::from_secs(10);

    let hold_duration = Duration::from_millis(config.hold_threshold_ms);
    let click_timeout = Duration::from_millis(280);

    loop {
        let now = Instant::now();

        // Process pending single-click Play/Pause
        if pending_click && !is_pressed {
            if let Some(t) = pending_click_time {
                if now.duration_since(t) >= click_timeout {
                    debug!("Dispatching Play/Pause click");
                    if let Err(e) = x11.fake_key(x11.space_keycode, true) {
                        debug!("Failed to press Space: {e}");
                    }
                    let _ = x11.fake_key(x11.space_keycode, false);
                    pending_click = false;
                    pending_click_time = None;
                }
            }
        }

        // Check active window
        let active_win = match x11.get_active_window() {
            Ok(Some(w)) => w,
            _ => {
                if accelerated {
                    restore_rate(&dbus_conn, original_rate).await;
                    accelerated = false;
                }
                is_pressed = false;
                pending_click = false;
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
        };

        if Some(active_win) != last_active_id {
            last_active_id = Some(active_win);
            is_vlc = x11.is_window_vlc(active_win);
            cached_geo = None;
        }

        if !is_vlc {
            if accelerated {
                restore_rate(&dbus_conn, original_rate).await;
                accelerated = false;
            }
            is_pressed = false;
            pending_click = false;
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        }

        // Cache geometry once every 500ms
        if cached_geo.is_none() || now.duration_since(last_geo_check) >= Duration::from_millis(500)
        {
            last_geo_check = now;
            cached_geo = x11.get_geometry(active_win).ok();
            is_fullscreen = x11.is_fullscreen(active_win);
        }

        let geo = match cached_geo {
            Some(g) => g,
            None => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };

        let pointer: PointerInfo = match x11.query_pointer(active_win) {
            Ok(p) => p,
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };

        // Check if cursor is within video area (excludes top titlebar and bottom controls)
        let button1_down = (pointer.mask & 0x0100/* Button1Mask */) != 0;
        let mut in_video_area = false;

        if pointer.win_x >= 0 && pointer.win_x < geo.width as i16 {
            if is_fullscreen {
                if pointer.win_y >= 0 && pointer.win_y < (geo.height as i16 - 60) {
                    in_video_area = true;
                }
            } else {
                if pointer.win_y >= 40 && pointer.win_y < (geo.height as i16 - 85) {
                    in_video_area = true;
                }
            }
        }

        if button1_down {
            if !is_pressed {
                if in_video_area {
                    is_pressed = true;
                    press_start = Some(now);
                    pending_click = false;
                }
            } else if let Some(start) = press_start {
                if !accelerated && now.duration_since(start) >= hold_duration {
                    debug!("Hold threshold met: accelerating VLC playback");
                    if let Some(rate) =
                        accelerate_playback(&dbus_conn, config.accelerated_rate).await
                    {
                        original_rate = rate;
                        accelerated = true;
                    }
                }
            }
        } else {
            // Button released
            if is_pressed {
                if accelerated {
                    debug!("Released after hold: restoring VLC playback rate");
                    restore_rate(&dbus_conn, original_rate).await;
                    accelerated = false;
                } else if let Some(start) = press_start {
                    if config.enable_single_tap && now.duration_since(start) < hold_duration {
                        pending_click = true;
                        pending_click_time = Some(now);
                    }
                }
                is_pressed = false;
                press_start = None;
            }
        }

        tokio::time::sleep(Duration::from_millis(16)).await;
    }
}

pub async fn accelerate_playback(
    dbus_conn: &Option<zbus::Connection>,
    target_rate: f64,
) -> Option<f64> {
    if let Some(conn) = dbus_conn {
        if let Ok(proxy) = MprisPlayerProxy::new(conn).await {
            if let Ok(status) = proxy.playback_status().await {
                if status == "Playing" {
                    let curr = proxy.rate().await.unwrap_or(1.0);
                    let orig = if (curr - target_rate).abs() > 0.05 {
                        curr
                    } else {
                        1.0
                    };
                    let _ = proxy.set_rate(target_rate).await;
                    return Some(orig);
                }
            }
        }
    }
    None
}

pub async fn restore_rate(dbus_conn: &Option<zbus::Connection>, rate: f64) {
    if let Some(conn) = dbus_conn {
        if let Ok(proxy) = MprisPlayerProxy::new(conn).await {
            let _ = proxy.set_rate(rate).await;
        }
    }
}
