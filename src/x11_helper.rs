use anyhow::{anyhow, Result};
use std::sync::Arc;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt as XprotoConnExt, Window};
use x11rb::protocol::xtest::*;
use x11rb::rust_connection::RustConnection;

#[allow(dead_code)]
pub struct X11Context {
    pub conn: Arc<RustConnection>,
    pub screen_num: usize,
    pub root: Window,
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
}
