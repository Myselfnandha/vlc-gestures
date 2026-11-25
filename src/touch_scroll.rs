use anyhow::Result;
use evdev::{AbsoluteAxisType, Device, EventType};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

use crate::config::TouchScrollConfig;
use crate::vlc;
use crate::x11_helper::{ScreenViewport, X11Context};

#[derive(Debug, Clone, Copy, Default)]
struct SlotInfo {
    active: bool,
    x: i32,
    y: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TwoFingerMode {
    None,
    Pinch,
    Scroll,
}

pub async fn run_touch_scroll(x11: Arc<X11Context>, config: TouchScrollConfig) -> Result<()> {
    if !config.enabled {
        info!("Touch scroll monitor disabled in configuration");
        return Ok(());
    }

    info!(
        "Starting Touch Scroll engine (Step: {}px, Natural: {}, TitlebarDrag: {}, HoldDrag: {})",
        config.scroll_step_px,
        config.natural_scroll,
        config.enable_titlebar_drag,
        config.enable_hold_to_drag
    );

    loop {
        let touch_devices = find_touch_devices();
        if touch_devices.is_empty() {
            debug!("No touch devices found; retrying in 3 seconds");
            tokio::time::sleep(Duration::from_secs(3)).await;
            continue;
        }

        info!("Discovered {} touch device(s)", touch_devices.len());

        let mut tasks = Vec::new();
        for dev_path in touch_devices {
            let x11_clone = Arc::clone(&x11);
            let cfg_clone = config.clone();
            tasks.push(tokio::spawn(async move {
                if let Err(e) = process_device(x11_clone, dev_path.clone(), cfg_clone).await {
                    debug!("Device {:?} disconnected: {e}", dev_path);
                }
            }));
        }

        for task in tasks {
            let _ = task.await;
        }

        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

fn find_touch_devices() -> Vec<PathBuf> {
    let mut matches = Vec::new();
    let entries = match std::fs::read_dir("/dev/input") {
        Ok(e) => e,
        Err(_) => return matches,
    };

    // First priority: target known ELAN touchscreen hardware
    for entry in entries.flatten() {
        let path = entry.path();
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with("event") {
                if let Ok(dev) = Device::open(&path) {
                    let dev_name = dev.name().unwrap_or("");
                    if dev_name.contains("ELAN2513") && !dev_name.contains("UNKNOWN") {
                        matches.push(path);
                        return matches;
                    }
                }
            }
        }
    }

    // Fallback: search for generic touchscreen devices (excluding mice and touchpads)
    let entries = match std::fs::read_dir("/dev/input") {
        Ok(e) => e,
        Err(_) => return matches,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with("event") {
                if let Ok(dev) = Device::open(&path) {
                    let dev_name = dev.name().unwrap_or("").to_lowercase();
                    if dev_name.contains("touch")
                        && !dev_name.contains("mouse")
                        && !dev_name.contains("pad")
                        && !dev_name.contains("unknown")
                    {
                        if let Some(abs_axes) = dev.supported_absolute_axes() {
                            let has_x = abs_axes.contains(AbsoluteAxisType::ABS_MT_POSITION_X);
                            let has_y = abs_axes.contains(AbsoluteAxisType::ABS_MT_POSITION_Y);
                            if has_x && has_y {
                                matches.push(path);
                            }
                        }
                    }
                }
            }
        }
    }
    matches
}

fn to_screen(
    raw_x: i32,
    raw_y: i32,
    min_x: i32,
    max_x: i32,
    min_y: i32,
    max_y: i32,
    vp: &ScreenViewport,
) -> (i16, i16) {
    let sx = if max_x > min_x {
        vp.x + (((raw_x - min_x) as f64 / (max_x - min_x) as f64) * vp.width as f64).round() as i16
    } else {
        vp.x + raw_x as i16
    };
    let sy = if max_y > min_y {
        vp.y + (((raw_y - min_y) as f64 / (max_y - min_y) as f64) * vp.height as f64).round() as i16
    } else {
        vp.y + raw_y as i16
    };

    let max_sx = vp.x + (vp.width as i16).saturating_sub(1);
    let max_sy = vp.y + (vp.height as i16).saturating_sub(1);

    (sx.clamp(vp.x, max_sx), sy.clamp(vp.y, max_sy))
}

async fn process_device(
    x11: Arc<X11Context>,
    path: PathBuf,
    config: TouchScrollConfig,
) -> Result<()> {
    let mut dev = Device::open(&path)?;
    let dev_name = dev.name().unwrap_or("Touchscreen").to_string();
    info!("Opened touch device: {:?} ({})", path, dev_name);

    // Acquire exclusive kernel grab so X11 libinput does not fight synthetic scrolling
    if let Err(e) = dev.grab() {
        warn!("Failed to grab input device {:?}: {e}", path);
    } else {
        info!("Successfully acquired exclusive grab on {:?}", path);
    }

    // Query hardware coordinate boundaries
    let (min_x, max_x, min_y, max_y) = match dev.get_abs_state() {
        Ok(abs_state) => {
            let abs_x = abs_state[AbsoluteAxisType::ABS_MT_POSITION_X.0 as usize];
            let abs_y = abs_state[AbsoluteAxisType::ABS_MT_POSITION_Y.0 as usize];
            (
                abs_x.minimum,
                if abs_x.maximum > 0 {
                    abs_x.maximum
                } else {
                    3408
                },
                abs_y.minimum,
                if abs_y.maximum > 0 {
                    abs_y.maximum
                } else {
                    1872
                },
            )
        }
        Err(_) => (0, 3408, 0, 1872),
    };

    info!(
        "Touchscreen calibration for {}: X=[{}..{}], Y=[{}..{}]",
        dev_name, min_x, max_x, min_y, max_y
    );

    let mut stream = dev.into_event_stream()?;
    let mut vp = x11.get_touchscreen_viewport();
    let mut last_vp_check = Instant::now();

    // D-Bus connection for VLC control
    let dbus_conn = zbus::connection::Connection::session().await.ok();

    let mut slots: HashMap<i32, SlotInfo> = HashMap::new();
    let mut current_slot = 0;

    // Single-finger tracking state
    let mut touch_active = false;
    let mut has_scrolled = false;
    let mut is_moving_window = false;
    let mut is_direct_dragging = false;
    let mut start_sx: i16 = 0;
    let mut start_sy: i16 = 0;
    let mut last_sx: i16 = 0;
    let mut last_sy: i16 = 0;
    let mut touch_down_time = Instant::now();
    let mut scroll_acc: i32 = 0;
    let mut scroll_h_acc: i32 = 0;
    let mut button1_is_down = false;
    let mut hold_drag_fired = false;
    let mut vlc_hold_fired = false;

    // Multi-finger guard (prevents rogue single clicks/jumps after multi-finger gestures)
    let mut multi_finger_active = false;

    // Two-finger tracking state
    let mut two_finger_in_progress = false;
    let mut two_start_time = Instant::now();
    let mut two_start_dist = 0.0;
    let mut two_last_dist = 0.0;
    let mut two_start_mid_x: i16 = 0;
    let mut two_start_mid_y: i16 = 0;
    let mut two_last_mid_x: i16 = 0;
    let mut two_last_mid_y: i16 = 0;
    let mut two_mode = TwoFingerMode::None;
    let mut two_scroll_acc: i32 = 0;
    let mut two_pinch_acc: f64 = 0.0;

    // VLC gesture state
    let mut is_vlc_touch = false;
    let mut vlc_hold_active = false;
    let mut vlc_original_rate = 1.0;
    let mut vlc_pending_tap = false;
    let mut vlc_pending_tap_time = Instant::now();

    loop {
        // Refresh viewport geometry periodically (every 5 seconds)
        if last_vp_check.elapsed() >= Duration::from_secs(5) {
            vp = x11.get_touchscreen_viewport();
            last_vp_check = Instant::now();
        }

        // Arm timers for hold-to-drag, VLC hold speedup, and VLC single tap
        let hold_deadline = if config.enable_hold_to_drag
            && touch_active
            && !has_scrolled
            && !is_moving_window
            && !is_direct_dragging
            && !multi_finger_active
            && !hold_drag_fired
        {
            Some(touch_down_time + Duration::from_millis(config.hold_drag_time_ms))
        } else {
            None
        };

        let vlc_hold_deadline = if is_vlc_touch
            && touch_active
            && !vlc_hold_active
            && !multi_finger_active
            && !vlc_hold_fired
        {
            Some(touch_down_time + Duration::from_millis(350))
        } else {
            None
        };

        let vlc_tap_deadline = if vlc_pending_tap && !touch_active {
            Some(vlc_pending_tap_time + Duration::from_millis(300))
        } else {
            None
        };

        tokio::select! {
            biased;

            // Timer 1: Hold-to-drag activation
            _ = async {
                if let Some(dl) = hold_deadline {
                    tokio::time::sleep_until(dl.into()).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                hold_drag_fired = true;
                if touch_active && !has_scrolled && !is_direct_dragging && !multi_finger_active {
                    let dist = f64::hypot((last_sx - start_sx) as f64, (last_sy - start_sy) as f64);
                    if dist < config.scroll_threshold_px as f64 {
                        is_direct_dragging = true;
                        let _ = x11.warp_pointer(start_sx, start_sy);
                        let _ = x11.fake_button(1, true);
                        button1_is_down = true;
                        debug!("Hold-to-drag activated at ({}, {})", start_sx, start_sy);
                    }
                }
            }

            // Timer 2: VLC touch hold speedup (1.5x)
            _ = async {
                if let Some(dl) = vlc_hold_deadline {
                    tokio::time::sleep_until(dl.into()).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                vlc_hold_fired = true;
                if is_vlc_touch && touch_active && !vlc_hold_active && !multi_finger_active {
                    let dist = f64::hypot((last_sx - start_sx) as f64, (last_sy - start_sy) as f64);
                    if dist < 60.0 {
                        if let Some(orig) = vlc::accelerate_playback(&dbus_conn, 1.5).await {
                            vlc_original_rate = orig;
                            vlc_hold_active = true;
                            debug!("VLC touch hold 1.5x speed activated");
                        }
                    }
                }
            }

            // Timer 3: VLC single tap play/pause expiration
            _ = async {
                if let Some(dl) = vlc_tap_deadline {
                    tokio::time::sleep_until(dl.into()).await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                vlc_pending_tap = false;
                vlc::trigger_play_pause(&dbus_conn, &x11).await;
                debug!("VLC touch single tap dispatched (Play/Pause)");
            }

            // Input event arrival
            event_result = stream.next_event() => {
                let ev = match event_result {
                    Ok(e) => e,
                    Err(e) => {
                        debug!("Input stream ended or error on {:?}: {e}", path);
                        break;
                    }
                };

                if ev.event_type() == EventType::ABSOLUTE {
                    match ev.code() {
                        0x2f /* ABS_MT_SLOT */ => {
                            current_slot = ev.value();
                        }
                        0x39 /* ABS_MT_TRACKING_ID */ => {
                            let slot = slots.entry(current_slot).or_default();
                            slot.active = ev.value() >= 0;
                        }
                        0x35 /* ABS_MT_POSITION_X */ => {
                            let slot = slots.entry(current_slot).or_default();
                            slot.x = ev.value();
                        }
                        0x36 /* ABS_MT_POSITION_Y */ => {
                            let slot = slots.entry(current_slot).or_default();
                            slot.y = ev.value();
                        }
                        _ => {}
                    }
                } else if ev.event_type() == EventType::SYNCHRONIZATION && ev.code() == 0 /* SYN_REPORT */ {
                    let active_slots: Vec<SlotInfo> = slots.values().filter(|s| s.active).copied().collect();
                    let n_fingers = active_slots.len();

                    // ----------------------------------------------------
                    // 0 FINGERS (ALL LIFTED)
                    // ----------------------------------------------------
                    if n_fingers == 0 {
                        // 1. VLC hold cleanup
                        if vlc_hold_active {
                            vlc::restore_rate(&dbus_conn, vlc_original_rate).await;
                            vlc_hold_active = false;
                            touch_active = false;
                            is_vlc_touch = false;
                        } else if is_vlc_touch {
                            let elapsed = touch_down_time.elapsed();
                            let dist = f64::hypot((last_sx - start_sx) as f64, (last_sy - start_sy) as f64);
                            if elapsed <= Duration::from_millis(350) && dist < 25.0 {
                                let now = Instant::now();
                                if vlc_pending_tap && now.duration_since(vlc_pending_tap_time) < Duration::from_millis(300) {
                                    vlc_pending_tap = false;
                                    let _ = x11.send_click_at(1, start_sx, start_sy);
                                    let _ = x11.send_click_at(1, start_sx, start_sy);
                                    debug!("VLC double tap dispatched (Toggle Fullscreen)");
                                } else {
                                    vlc_pending_tap = true;
                                    vlc_pending_tap_time = now;
                                }
                            }
                            touch_active = false;
                            is_vlc_touch = false;
                        }

                        // 2. Mouse button 1 release
                        if button1_is_down {
                            let _ = x11.fake_button(1, false);
                            button1_is_down = false;
                        }

                        // 3. Titlebar move finish
                        if is_moving_window {
                            let dist = f64::hypot((last_sx - start_sx) as f64, (last_sy - start_sy) as f64);
                            let elapsed = touch_down_time.elapsed();
                            if dist < 8.0 && elapsed <= Duration::from_millis(config.tap_max_time_ms) {
                                let _ = x11.send_click_at(1, start_sx, start_sy);
                            }
                            is_moving_window = false;
                        }

                        // 4. Direct drag finish
                        if is_direct_dragging {
                            is_direct_dragging = false;
                        } else if touch_active && !has_scrolled && !multi_finger_active {
                            // 5. Normal single-finger tap
                            let elapsed = touch_down_time.elapsed();
                            let dist = f64::hypot((last_sx - start_sx) as f64, (last_sy - start_sy) as f64);
                            if elapsed <= Duration::from_millis(config.tap_max_time_ms)
                                && dist < config.scroll_threshold_px as f64
                            {
                                debug!("Single tap dispatched at ({}, {})", start_sx, start_sy);
                                let _ = x11.send_click_at(1, start_sx, start_sy);
                            }
                        }

                        // 6. Two-finger tap release (Right Click)
                        if two_finger_in_progress && two_mode == TwoFingerMode::None {
                            if config.two_finger_right_click {
                                let elapsed = two_start_time.elapsed();
                                let dist_moved = f64::hypot(
                                    (two_last_mid_x - two_start_mid_x) as f64,
                                    (two_last_mid_y - two_start_mid_y) as f64,
                                );
                                if elapsed <= Duration::from_millis(450) && dist_moved < 25.0 {
                                    debug!(
                                        "Two-finger tap dispatched (Right Click) at ({}, {})",
                                        two_last_mid_x, two_last_mid_y
                                    );
                                    let _ = x11.send_click_at(3, two_last_mid_x, two_last_mid_y);
                                }
                            }
                        }

                        // Reset all states
                        touch_active = false;
                        has_scrolled = false;
                        two_finger_in_progress = false;
                        two_mode = TwoFingerMode::None;
                        multi_finger_active = false;
                        hold_drag_fired = false;
                        vlc_hold_fired = false;

                    // ----------------------------------------------------
                    // 1 FINGER (TAP, SCROLL, OR DRAG)
                    // ----------------------------------------------------
                    } else if n_fingers == 1 {
                        if multi_finger_active {
                            continue;
                        }

                        let f = active_slots[0];
                        let (sx, sy) = to_screen(f.x, f.y, min_x, max_x, min_y, max_y, &vp);

                        if !touch_active {
                            let _ = x11.warp_pointer(sx, sy);
                            is_vlc_touch = x11.is_on_vlc_video(sx, sy);
                            if is_vlc_touch {
                                is_moving_window = false;
                            } else if config.enable_titlebar_drag
                                && x11.is_on_titlebar(sx, sy, config.titlebar_height_px as i16)
                            {
                                is_moving_window = true;
                                let _ = x11.fake_button(1, true);
                                button1_is_down = true;
                                debug!("Titlebar drag initiated at ({}, {})", sx, sy);
                            } else {
                                is_moving_window = false;
                            }

                            touch_active = true;
                            has_scrolled = false;
                            is_direct_dragging = false;
                            hold_drag_fired = false;
                            vlc_hold_fired = false;
                            start_sx = sx;
                            start_sy = sy;
                            last_sx = sx;
                            last_sy = sy;
                            touch_down_time = Instant::now();
                            scroll_acc = 0;
                            scroll_h_acc = 0;
                        } else {
                            if is_vlc_touch {
                                last_sx = sx;
                                last_sy = sy;
                                continue;
                            }

                            if is_moving_window || is_direct_dragging {
                                let _ = x11.warp_pointer(sx, sy);
                                last_sx = sx;
                                last_sy = sy;
                            } else {
                                let dx = (sx - start_sx).abs() as f64;
                                let dy = (sy - start_sy).abs() as f64;
                                let dist = f64::hypot(dx, dy);
                                let scroll_thresh = config.scroll_threshold_px as f64;

                                if !has_scrolled {
                                    if dist >= scroll_thresh
                                        && (dy >= scroll_thresh * 0.6 || dx >= scroll_thresh * 0.6)
                                    {
                                        has_scrolled = true;
                                        last_sx = sx;
                                        last_sy = sy;
                                    }
                                }

                                if has_scrolled {
                                    let delta_y = (sy - last_sy) as i32;
                                    let delta_x = (sx - last_sx) as i32;
                                    last_sx = sx;
                                    last_sy = sy;

                                    let _ = x11.warp_pointer(sx, sy);

                                    // Axis-Locking: suppress slight horizontal drift during vertical scrolling
                                    if delta_y.abs() as f64 >= delta_x.abs() as f64 * 1.3 {
                                        scroll_acc += delta_y;
                                        scroll_h_acc = 0;
                                    } else if delta_x.abs() as f64 >= delta_y.abs() as f64 * 1.3 {
                                        scroll_h_acc += delta_x;
                                        scroll_acc = 0;
                                    } else {
                                        scroll_acc += delta_y;
                                        scroll_h_acc += delta_x;
                                    }

                                    let step = config.scroll_step_px;
                                    while scroll_acc.abs() >= step {
                                        let btn = if config.natural_scroll {
                                            if scroll_acc < 0 { 5 } else { 4 }
                                        } else {
                                            if scroll_acc < 0 { 4 } else { 5 }
                                        };
                                        let _ = x11.fake_click(btn);
                                        if scroll_acc < 0 {
                                            scroll_acc += step;
                                        } else {
                                            scroll_acc -= step;
                                        }
                                    }

                                    while scroll_h_acc.abs() >= step {
                                        let btn = if config.natural_scroll {
                                            if scroll_h_acc < 0 { 7 } else { 6 }
                                        } else {
                                            if scroll_h_acc < 0 { 6 } else { 7 }
                                        };
                                        let _ = x11.fake_click(btn);
                                        if scroll_h_acc < 0 {
                                            scroll_h_acc += step;
                                        } else {
                                            scroll_h_acc -= step;
                                        }
                                    }
                                } else {
                                    last_sx = sx;
                                    last_sy = sy;
                                }
                            }
                        }

                    // ----------------------------------------------------
                    // 2 FINGERS (PINCH-ZOOM, 2-FINGER SCROLL, 2-FINGER TAP)
                    // ----------------------------------------------------
                    } else if n_fingers == 2 {
                        multi_finger_active = true;
                        if button1_is_down {
                            let _ = x11.fake_button(1, false);
                            button1_is_down = false;
                        }
                        is_moving_window = false;
                        is_direct_dragging = false;
                        touch_active = false;

                        let f0 = active_slots[0];
                        let f1 = active_slots[1];
                        let (sx0, sy0) = to_screen(f0.x, f0.y, min_x, max_x, min_y, max_y, &vp);
                        let (sx1, sy1) = to_screen(f1.x, f1.y, min_x, max_x, min_y, max_y, &vp);
                        let dist = f64::hypot((sx1 - sx0) as f64, (sy1 - sy0) as f64);
                        let mid_x = (sx0 + sx1) / 2;
                        let mid_y = (sy0 + sy1) / 2;

                        if !two_finger_in_progress {
                            two_finger_in_progress = true;
                            two_start_time = Instant::now();
                            two_start_dist = dist;
                            two_last_dist = dist;
                            two_start_mid_x = mid_x;
                            two_start_mid_y = mid_y;
                            two_last_mid_x = mid_x;
                            two_last_mid_y = mid_y;
                            two_mode = TwoFingerMode::None;
                            two_scroll_acc = 0;
                            two_pinch_acc = 0.0;
                        }

                        let dist_change = (dist - two_start_dist).abs();
                        let drag_dist = f64::hypot(
                            (mid_x - two_start_mid_x) as f64,
                            (mid_y - two_start_mid_y) as f64,
                        );

                        if two_mode == TwoFingerMode::None {
                            if dist_change > 30.0 && config.enable_pinch_zoom {
                                two_mode = TwoFingerMode::Pinch;
                                two_last_dist = dist;
                            } else if drag_dist > config.scroll_threshold_px as f64
                                && config.enable_two_finger_scroll
                            {
                                two_mode = TwoFingerMode::Scroll;
                                two_last_mid_x = mid_x;
                                two_last_mid_y = mid_y;
                            }
                        }

                        if two_mode == TwoFingerMode::Pinch && config.enable_pinch_zoom {
                            let delta_d = dist - two_last_dist;
                            two_last_dist = dist;
                            two_pinch_acc += delta_d;
                            let pinch_step = config.pinch_zoom_step_px;

                            while two_pinch_acc.abs() >= pinch_step {
                                let zoom_in = two_pinch_acc > 0.0;
                                let _ = x11.send_zoom(zoom_in);
                                if two_pinch_acc > 0.0 {
                                    two_pinch_acc -= pinch_step;
                                } else {
                                    two_pinch_acc += pinch_step;
                                }
                            }
                        } else if two_mode == TwoFingerMode::Scroll
                            && config.enable_two_finger_scroll
                        {
                            let delta_y = (mid_y - two_last_mid_y) as i32;
                            two_last_mid_y = mid_y;
                            two_last_mid_x = mid_x;
                            two_scroll_acc += delta_y;
                            let step = config.scroll_step_px;

                            let _ = x11.warp_pointer(mid_x, mid_y);
                            while two_scroll_acc.abs() >= step {
                                let btn = if config.natural_scroll {
                                    if two_scroll_acc < 0 { 5 } else { 4 }
                                } else {
                                    if two_scroll_acc < 0 { 4 } else { 5 }
                                };
                                let _ = x11.fake_click(btn);
                                if two_scroll_acc < 0 {
                                    two_scroll_acc += step;
                                } else {
                                    two_scroll_acc -= step;
                                }
                            }
                        }

                    // ----------------------------------------------------
                    // 3+ FINGERS
                    // ----------------------------------------------------
                    } else if n_fingers >= 3 {
                        multi_finger_active = true;
                        if button1_is_down {
                            let _ = x11.fake_button(1, false);
                            button1_is_down = false;
                        }
                        is_moving_window = false;
                        is_direct_dragging = false;
                        touch_active = false;
                    }
                }
            }
        }
    }

    if button1_is_down {
        let _ = x11.fake_button(1, false);
    }
    let _ = stream.device_mut().ungrab();
    Ok(())
}
