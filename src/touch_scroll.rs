use anyhow::Result;
use evdev::{AbsoluteAxisType, Device, EventType};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, info};

use crate::config::TouchScrollConfig;
use crate::x11_helper::X11Context;

pub async fn run_touch_scroll(x11: Arc<X11Context>, config: TouchScrollConfig) -> Result<()> {
    if !config.enabled {
        info!("Touch scroll monitor disabled in configuration");
        return Ok(());
    }

    info!(
        "Starting Touch Scroll engine (Step: {}px, Natural: {})",
        config.scroll_step_px, config.natural_scroll
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

    for entry in entries.flatten() {
        let path = entry.path();
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with("event") {
                if let Ok(dev) = Device::open(&path) {
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
    matches
}

async fn process_device(
    x11: Arc<X11Context>,
    path: PathBuf,
    config: TouchScrollConfig,
) -> Result<()> {
    let dev = Device::open(&path)?;
    info!(
        "Opened touch device: {:?} ({})",
        path,
        dev.name().unwrap_or("Unknown")
    );

    let mut stream = dev.into_event_stream()?;
    let mut fingers: HashMap<i32, (i32, i32)> = HashMap::new();
    let mut active_slot = 0;
    let mut initial_y: Option<i32> = None;
    let mut last_scroll_y: Option<i32> = None;
    let mut accumulated_scroll: i32 = 0;

    let mut touch_start_time = Instant::now();
    let mut is_two_finger_tap = false;

    while let Ok(ev) = stream.next_event().await {
        if ev.event_type() == EventType::ABSOLUTE {
            match ev.code() {
                0x2f /* ABS_MT_SLOT */ => {
                    active_slot = ev.value();
                }
                0x39 /* ABS_MT_TRACKING_ID */ => {
                    if ev.value() == -1 {
                        fingers.remove(&active_slot);
                        if fingers.is_empty() {
                            // Check for two-finger tap right click
                            if is_two_finger_tap && config.enable_two_finger_scroll {
                                if touch_start_time.elapsed() < Duration::from_millis(300) {
                                    debug!("Synthesizing two-finger tap (Right Click)");
                                    let _ = x11.fake_button(3, true);
                                    let _ = x11.fake_button(3, false);
                                }
                            }
                            initial_y = None;
                            last_scroll_y = None;
                            accumulated_scroll = 0;
                            is_two_finger_tap = false;
                        }
                    } else {
                        fingers.insert(active_slot, (0, 0));
                        if fingers.len() == 1 {
                            touch_start_time = Instant::now();
                        } else if fingers.len() == 2 {
                            is_two_finger_tap = true;
                        } else {
                            is_two_finger_tap = false;
                        }
                    }
                }
                0x35 /* ABS_MT_POSITION_X */ => {
                    if let Some(pt) = fingers.get_mut(&active_slot) {
                        pt.0 = ev.value();
                    }
                }
                0x36 /* ABS_MT_POSITION_Y */ => {
                    let y = ev.value();
                    if let Some(pt) = fingers.get_mut(&active_slot) {
                        pt.1 = y;
                    }

                    if fingers.len() == 1 {
                        if initial_y.is_none() {
                            initial_y = Some(y);
                            last_scroll_y = Some(y);
                        } else if let Some(last_y) = last_scroll_y {
                            let diff = y - last_y;
                            accumulated_scroll += diff;

                            if accumulated_scroll.abs() >= config.scroll_step_px {
                                let steps = accumulated_scroll / config.scroll_step_px;
                                accumulated_scroll %= config.scroll_step_px;
                                last_scroll_y = Some(y);

                                // Button 4 is Wheel Up, Button 5 is Wheel Down
                                let btn = if config.natural_scroll {
                                    if steps > 0 { 5 } else { 4 }
                                } else {
                                    if steps > 0 { 4 } else { 5 }
                                };

                                for _ in 0..steps.abs() {
                                    let _ = x11.fake_button(btn, true);
                                    let _ = x11.fake_button(btn, false);
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}
