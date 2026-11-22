use anyhow::{anyhow, Result};
use std::sync::Arc;
use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as RandrConnExt;
use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt as XprotoConnExt, Window};
use x11rb::protocol::xtest::*;
use x11rb::rust_connection::RustConnection;

#[allow(dead_code)]
pub struct X11Context {
    pub conn: Arc<RustConnection>,
    pub screen_num: usize,
    pub root: Window,
    pub width_in_pixels: u16,
    pub height_in_pixels: u16,
    pub net_active_window: Atom,
    pub net_wm_state: Atom,
    pub net_wm_state_fullscreen: Atom,
    pub primary: Atom,
    pub space_keycode: u8,
    pub ctrl_l_keycode: u8,
    pub c_keycode: u8,
    pub v_keycode: u8,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct ScreenViewport {
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct WindowGeometry {
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct PointerInfo {
    pub win_x: i16,
    pub win_y: i16,
    pub root_x: i16,
    pub root_y: i16,
    pub mask: u16,
}

impl X11Context {
    pub fn new() -> Result<Self> {
        let (conn, screen_num) =
            x11rb::connect(None).map_err(|e| anyhow!("Failed to connect to X11 display: {e}"))?;
        let conn = Arc::new(conn);

        let setup = conn.setup();
        let screen = setup
            .roots
            .get(screen_num)
            .ok_or_else(|| anyhow!("No screen found at index {screen_num}"))?;
        let root = screen.root;
        let width_in_pixels = screen.width_in_pixels;
        let height_in_pixels = screen.height_in_pixels;

        let net_active_cookie = conn.intern_atom(false, b"_NET_ACTIVE_WINDOW")?;
        let net_state_cookie = conn.intern_atom(false, b"_NET_WM_STATE")?;
        let net_fs_cookie = conn.intern_atom(false, b"_NET_WM_STATE_FULLSCREEN")?;
        let primary_cookie = conn.intern_atom(false, b"PRIMARY")?;

        let net_active_window = net_active_cookie.reply()?.atom;
        let net_wm_state = net_state_cookie.reply()?.atom;
        let net_wm_state_fullscreen = net_fs_cookie.reply()?.atom;
        let primary = primary_cookie.reply()?.atom;

        let space_keycode = Self::find_keycode(&conn, 0x0020).unwrap_or(65);
        let ctrl_l_keycode = Self::find_keycode(&conn, 0xffe3).unwrap_or(37);
        let c_keycode = Self::find_keycode(&conn, 0x0063).unwrap_or(54);
        let v_keycode = Self::find_keycode(&conn, 0x0076).unwrap_or(55);

        Ok(Self {
            conn,
            screen_num,
            root,
            width_in_pixels,
            height_in_pixels,
            net_active_window,
            net_wm_state,
            net_wm_state_fullscreen,
            primary,
            space_keycode,
            ctrl_l_keycode,
            c_keycode,
            v_keycode,
        })
    }

    fn find_keycode(conn: &RustConnection, target_keysym: u32) -> Option<u8> {
        let setup = conn.setup();
        let min = setup.min_keycode;
        let max = setup.max_keycode;
        let count = max.saturating_sub(min) + 1;
        let mapping = conn.get_keyboard_mapping(min, count).ok()?.reply().ok()?;

        let keysyms_per_code = mapping.keysyms_per_keycode as usize;
        for (idx, chunk) in mapping.keysyms.chunks(keysyms_per_code).enumerate() {
            let keycode = min + idx as u8;
            for &ks in chunk {
                if ks == target_keysym {
                    return Some(keycode);
                }
            }
        }
        None
    }

    pub fn get_active_window(&self) -> Result<Option<Window>> {
        let reply = self
            .conn
            .get_property(
                false,
                self.root,
                self.net_active_window,
                AtomEnum::WINDOW,
                0,
                1,
            )?
            .reply()?;

        if reply.value.len() >= 4 {
            let val = u32::from_ne_bytes([
                reply.value[0],
                reply.value[1],
                reply.value[2],
                reply.value[3],
            ]);
            if val != 0 {
                return Ok(Some(val));
            }
        }
        Ok(None)
    }

    pub fn is_window_vlc(&self, win: Window) -> bool {
        if let Ok(reply) =
            self.conn
                .get_property(false, win, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 128)
        {
            if let Ok(prop) = reply.reply() {
                if let Ok(text) = std::str::from_utf8(&prop.value) {
                    return text.to_lowercase().contains("vlc");
                }
            }
        }
        false
    }

    pub fn get_geometry(&self, win: Window) -> Result<WindowGeometry> {
        let geom = self.conn.get_geometry(win)?.reply()?;
        Ok(WindowGeometry {
            x: geom.x,
            y: geom.y,
            width: geom.width,
            height: geom.height,
        })
    }

    #[allow(clippy::chunks_exact_to_as_chunks)]
    pub fn is_fullscreen(&self, win: Window) -> bool {
        if let Ok(reply) =
            self.conn
                .get_property(false, win, self.net_wm_state, AtomEnum::ATOM, 0, 32)
        {
            if let Ok(prop) = reply.reply() {
                for chunk in prop.value.chunks_exact(4) {
                    let atom = u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                    if atom == self.net_wm_state_fullscreen {
                        return true;
                    }
                }
            }
        }
        false
    }

    pub fn query_pointer(&self, win: Window) -> Result<PointerInfo> {
        let q = self.conn.query_pointer(win)?.reply()?;
        Ok(PointerInfo {
            win_x: q.win_x,
            win_y: q.win_y,
            root_x: q.root_x,
            root_y: q.root_y,
            mask: u16::from(q.mask),
        })
    }

    pub fn fake_key(&self, keycode: u8, is_press: bool) -> Result<()> {
        let detail = keycode;
        let event_type = if is_press {
            2 /* KeyPress */
        } else {
            3 /* KeyRelease */
        };
        fake_input(&*self.conn, event_type, detail, 0, self.root, 0, 0, 0)?;
        self.conn.flush()?;
        Ok(())
    }

    pub fn fake_button(&self, button: u8, is_press: bool) -> Result<()> {
        let detail = button;
        let event_type = if is_press {
            4 /* ButtonPress */
        } else {
            5 /* ButtonRelease */
        };
        fake_input(&*self.conn, event_type, detail, 0, self.root, 0, 0, 0)?;
        self.conn.flush()?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn fake_motion(&self, root_x: i16, root_y: i16) -> Result<()> {
        fake_input(
            &*self.conn,
            6, /* MotionNotify */
            0,
            0,
            self.root,
            root_x,
            root_y,
            0,
        )?;
        self.conn.flush()?;
        Ok(())
    }

    pub fn has_primary_selection(&self) -> bool {
        if let Ok(reply) = self.conn.get_selection_owner(self.primary) {
            if let Ok(owner) = reply.reply() {
                return owner.owner != 0;
            }
        }
        false
    }

    pub fn send_key_combo(&self, mod_keycode: u8, keycode: u8) -> Result<()> {
        self.fake_key(mod_keycode, true)?;
        self.fake_key(keycode, true)?;
        self.fake_key(keycode, false)?;
        self.fake_key(mod_keycode, false)?;
        Ok(())
    }

    pub fn warp_pointer(&self, x: i16, y: i16) -> Result<()> {
        self.conn
            .warp_pointer(x11rb::NONE, self.root, 0, 0, 0, 0, x, y)?;
        self.conn.flush()?;
        Ok(())
    }

    pub fn fake_click(&self, button: u8) -> Result<()> {
        self.fake_button(button, true)?;
        self.fake_button(button, false)?;
        Ok(())
    }

    pub fn send_click_at(&self, button: u8, x: i16, y: i16) -> Result<()> {
        self.warp_pointer(x, y)?;
        self.fake_click(button)
    }

    pub fn send_zoom(&self, zoom_in: bool) -> Result<()> {
        let btn = if zoom_in { 4 } else { 5 };
        self.fake_key(self.ctrl_l_keycode, true)?;
        let _ = self.fake_button(btn, true);
        let _ = self.fake_button(btn, false);
        self.fake_key(self.ctrl_l_keycode, false)?;
        Ok(())
    }

    pub fn get_touchscreen_viewport(&self) -> ScreenViewport {
        if let Ok(res_cookie) = self.conn.randr_get_screen_resources_current(self.root) {
            if let Ok(res) = res_cookie.reply() {
                for &output in &res.outputs {
                    if let Ok(out_cookie) = self
                        .conn
                        .randr_get_output_info(output, res.config_timestamp)
                    {
                        if let Ok(out_info) = out_cookie.reply() {
                            let name = String::from_utf8_lossy(&out_info.name);
                            if (name.contains("eDP") || name.contains("LVDS"))
                                && out_info.crtc != 0
                                && out_info.crtc != x11rb::NONE
                            {
                                if let Ok(crtc_cookie) = self
                                    .conn
                                    .randr_get_crtc_info(out_info.crtc, res.config_timestamp)
                                {
                                    if let Ok(crtc) = crtc_cookie.reply() {
                                        return ScreenViewport {
                                            x: crtc.x,
                                            y: crtc.y,
                                            width: crtc.width,
                                            height: crtc.height,
                                        };
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        ScreenViewport {
            x: 0,
            y: 0,
            width: self.width_in_pixels,
            height: self.height_in_pixels,
        }
    }

    pub fn is_on_titlebar(&self, sx: i16, sy: i16, titlebar_height: i16) -> bool {
        if let Ok(reply) = self.conn.query_pointer(self.root) {
            if let Ok(q) = reply.reply() {
                if q.child != 0 && q.child != x11rb::NONE {
                    if let Ok(geom_reply) = self.conn.get_geometry(q.child) {
                        if let Ok(geom) = geom_reply.reply() {
                            // Skip maximized windows / panels that span full display width
                            if geom.width as u32 >= (self.width_in_pixels as u32).saturating_sub(10)
                            {
                                return false;
                            }
                            if geom.height <= 50 {
                                return false;
                            }
                            let gx = geom.x;
                            let gy = geom.y;
                            let gw = geom.width as i16;
                            if sx >= gx && sx <= gx + gw && sy >= gy && sy <= gy + titlebar_height {
                                return true;
                            }
                        }
                    }
                }
            }
        }
        false
    }

    pub fn is_on_vlc_video(&self, sx: i16, sy: i16) -> bool {
        if let Ok(Some(win)) = self.get_active_window() {
            if self.is_window_vlc(win) {
                if let Ok(geo) = self.get_geometry(win) {
                    if let Ok(reply) = self.conn.translate_coordinates(self.root, win, sx, sy) {
                        if let Ok(coords) = reply.reply() {
                            let is_fullscreen = self.is_fullscreen(win);
                            let win_x = coords.dst_x;
                            let win_y = coords.dst_y;
                            if win_x >= 0 && (win_x as u16) < geo.width {
                                if is_fullscreen {
                                    return win_y >= 0
                                        && (win_y as u16) < geo.height.saturating_sub(60);
                                } else {
                                    return win_y >= 40
                                        && (win_y as u16) < geo.height.saturating_sub(85);
                                }
                            }
                        }
                    }
                }
            }
        }
        false
    }
}
