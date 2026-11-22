use anyhow::Result;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info};

use crate::config::SmartTapConfig;
use crate::x11_helper::X11Context;

pub async fn run_smart_tap(x11: Arc<X11Context>, config: SmartTapConfig) -> Result<()> {
    if !config.enabled {
        info!("Smart 3-Finger Tap monitor disabled in configuration");
        return Ok(());
    }

    info!("Starting Smart 3-Finger Tap engine (Trigger Button: {})", config.trigger_button);

    // Apply xinput button map so 3-finger tap emits the configured trigger button
    configure_touchpad_map(&config.device_name_pattern, config.trigger_button, true);

    // X11 polling / event listener for button state
    let target_button_mask = 1 << (config.trigger_button - 1);
    let mut was_down = false;

    loop {
        if let Ok(Some(active_win)) = x11.get_active_window() {
            if let Ok(pointer) = x11.query_pointer(active_win) {
                let is_down = (pointer.mask & target_button_mask as u16) != 0;

                if is_down && !was_down {
                    was_down = true;
                    handle_smart_tap(&x11);
                } else if !is_down && was_down {
                    was_down = false;
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn handle_smart_tap(x11: &X11Context) {
    let has_selection = x11.has_primary_selection();
    if has_selection {
        debug!("Smart Tap: text selection detected -> Emitting Copy (Ctrl+C)");
        let _ = x11.send_key_combo(x11.ctrl_l_keycode, x11.c_keycode);
    } else {
        debug!("Smart Tap: no selection -> Emitting Paste (Ctrl+V)");
        let _ = x11.send_key_combo(x11.ctrl_l_keycode, x11.v_keycode);
    }
}

pub fn configure_touchpad_map(device_pattern: &str, target_button: u8, enable: bool) {
    let target = if enable {
        target_button.to_string()
    } else {
        "2".to_string()
    };

    let target_map = ["1", &target, "3", "4", "5", "6", "7"];

    if let Ok(output) = Command::new("xinput").args(["list", "--short"]).output() {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            if line.contains(device_pattern) || (line.contains("Touchpad") && !line.contains("XTEST")) {
                if let Some(id_part) = line.split("id=").nth(1) {
                    if let Some(id_str) = id_part.split_whitespace().next() {
                        let mut cmd = Command::new("xinput");
                        cmd.arg("set-button-map").arg(id_str);
                        for m in &target_map {
                            cmd.arg(m);
                        }
                        let _ = cmd.output();
                        info!("Touchpad {} remapped button 2 -> {}", id_str, target);
                    }
                }
            }
        }
    }
}
