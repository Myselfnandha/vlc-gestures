#!/usr/bin/env python3
import os
import sys
import glob
import time
import threading
import signal
import fcntl
import dbus
from dbus.mainloop.glib import DBusGMainLoop
from gi.repository import GLib
from Xlib import X, display, XK
from Xlib.ext import xtest

LOCK_FILE = '/tmp/vlc-hold-speed.lock'
lock_fd = None

def acquire_lock():
    global lock_fd
    try:
        lock_fd = open(LOCK_FILE, 'w')
        fcntl.flock(lock_fd.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        return True
    except (IOError, BlockingIOError):
        return False

def find_xauthority():
    home_xauth = os.path.expanduser("~/.Xauthority")
    candidates = [home_xauth]
    uid = os.getuid()
    candidates += glob.glob(f"/run/user/{uid}/xauth_*")
    candidates += glob.glob(f"/run/user/{uid}/*xauth*")
    candidates += glob.glob("/tmp/sddm-auth-*")
    candidates += glob.glob("/tmp/xauth_*")
    for c in candidates:
        if os.path.isfile(c) and os.access(c, os.R_OK):
            return c
    return None

def setup_env():
    if not os.environ.get("DISPLAY"):
        os.environ["DISPLAY"] = ":0.0"
    xauth = os.environ.get("XAUTHORITY")
    if not xauth or not os.path.exists(xauth):
        found = find_xauthority()
        if found:
            os.environ["XAUTHORITY"] = found

class VlcGestureManager:
    def __init__(self):
        self.stop_event = threading.Event()
        self.worker_thread = None
        self.bus = None

    def start_worker(self):
        if self.worker_thread and self.worker_thread.is_alive():
            return
        self.stop_event.clear()
        self.worker_thread = threading.Thread(target=self._run_loop, daemon=True)
        self.worker_thread.start()

    def stop_worker(self):
        self.stop_event.set()
        if self.worker_thread and self.worker_thread.is_alive():
            self.worker_thread.join(timeout=1.0)
        self.worker_thread = None

    def _run_loop(self):
        setup_env()
        d = None
        while not self.stop_event.is_set():
            try:
                d = display.Display()
                break
            except Exception:
                setup_env()
                time.sleep(0.3)

        if d is None:
            return

        root = d.screen().root
        net_active = d.intern_atom('_NET_ACTIVE_WINDOW')
        state_fullscreen = d.intern_atom('_NET_WM_STATE_FULLSCREEN')
        wm_state = d.intern_atom('_NET_WM_STATE')
        space_code = d.keysym_to_keycode(XK.string_to_keysym('space'))

        bus = dbus.SessionBus()

        is_pressed = False
        press_start_time = 0.0
        accelerated = False
        original_rate = 1.0
        pending_click = False
        pending_click_time = 0.0
        is_double_click = False
        player_props = None

        last_active_id = None
        is_vlc = False
        vlc_win = None
        cached_geo = None
        is_fullscreen = False
        last_geo_check = 0.0

        def get_player_interfaces():
            try:
                obj = bus.get_object('org.mpris.MediaPlayer2.vlc', '/org/mpris/MediaPlayer2')
                props = dbus.Interface(obj, 'org.freedesktop.DBus.Properties')
                player = dbus.Interface(obj, 'org.mpris.MediaPlayer2.Player')
                return props, player
            except Exception:
                return None, None

        while not self.stop_event.is_set():
            try:
                now = time.time()

                if pending_click and not is_pressed:
                    if now - pending_click_time >= 0.30:
                        try:
                            xtest.fake_input(d, X.KeyPress, space_code)
                            d.sync()
                            xtest.fake_input(d, X.KeyRelease, space_code)
                            d.sync()
                        except Exception:
                            props, player = get_player_interfaces()
                            if player:
                                try:
                                    player.PlayPause()
                                except Exception:
                                    pass
                        pending_click = False

                active_prop = root.get_full_property(net_active, X.AnyPropertyType)
                if not active_prop or not active_prop.value:
                    if accelerated and player_props:
                        try:
                            player_props.Set('org.mpris.MediaPlayer2.Player', 'Rate', dbus.Double(original_rate))
                        except Exception:
                            pass
                        accelerated = False
                    is_pressed = False
                    pending_click = False
                    time.sleep(0.2)
                    continue

                active_id = active_prop.value[0]
                if active_id != last_active_id:
                    last_active_id = active_id
                    try:
                        vlc_win = d.create_resource_object('window', active_id)
                        wm_class = vlc_win.get_wm_class()
                        is_vlc = bool(wm_class and 'vlc' in [c.lower() for c in wm_class])
                    except Exception:
                        is_vlc = False
                        vlc_win = None
                    last_geo_check = 0.0

                if not is_vlc or vlc_win is None:
                    if accelerated and player_props:
                        try:
                            player_props.Set('org.mpris.MediaPlayer2.Player', 'Rate', dbus.Double(original_rate))
                        except Exception:
                            pass
                        accelerated = False
                    is_pressed = False
                    pending_click = False
                    time.sleep(0.2)
                    continue

                if now - last_geo_check >= 1.0 or cached_geo is None:
                    last_geo_check = now
                    try:
                        cached_geo = vlc_win.get_geometry()
                        state_prop = vlc_win.get_full_property(wm_state, X.AnyPropertyType)
                        if state_prop:
                            is_fullscreen = state_fullscreen in state_prop.value
                        else:
                            is_fullscreen = False
                    except Exception:
                        cached_geo = None

                if cached_geo is None:
                    time.sleep(0.1)
                    continue

                q = vlc_win.query_pointer()
                button1_down = bool(q.mask & X.Button1Mask)

                in_video_area = False
                if 0 <= q.win_x < cached_geo.width:
                    if is_fullscreen:
                        if q.win_y < cached_geo.height - 60:
                            in_video_area = True
                    else:
                        if 40 <= q.win_y < (cached_geo.height - 85):
                            in_video_area = True

                now = time.time()

                if button1_down:
                    if not is_pressed:
                        if in_video_area:
                            is_pressed = True
                            press_start_time = now
                            if pending_click and (now - pending_click_time < 0.30):
                                pending_click = False
                                is_double_click = True
                            else:
                                is_double_click = False
                    else:
                        if not accelerated and (now - press_start_time >= 0.35):
                            pending_click = False
                            props, player = get_player_interfaces()
                            player_props = props
                            if player_props:
                                try:
                                    status = str(player_props.Get('org.mpris.MediaPlayer2.Player', 'PlaybackStatus'))
                                    if status == 'Playing':
                                        curr_rate = float(player_props.Get('org.mpris.MediaPlayer2.Player', 'Rate'))
                                        original_rate = curr_rate if curr_rate != 1.5 else 1.0
                                        player_props.Set('org.mpris.MediaPlayer2.Player', 'Rate', dbus.Double(1.5))
                                        accelerated = True
                                except Exception:
                                    pass
                else:
                    if is_pressed:
                        if accelerated:
                            if player_props:
                                try:
                                    player_props.Set('org.mpris.MediaPlayer2.Player', 'Rate', dbus.Double(original_rate))
                                except Exception:
                                    pass
                            accelerated = False
                        elif is_double_click:
                            is_double_click = False
                        else:
                            if now - press_start_time < 0.35:
                                pending_click = True
                                pending_click_time = now
                        is_pressed = False

                if button1_down:
                    time.sleep(0.025)
                else:
                    time.sleep(0.05)

            except Exception:
                time.sleep(0.1)

        if accelerated and player_props:
            try:
                player_props.Set('org.mpris.MediaPlayer2.Player', 'Rate', dbus.Double(original_rate))
            except Exception:
                pass

def main():
    if not acquire_lock():
        sys.exit(0)

    setup_env()

    DBusGMainLoop(set_as_default=True)
    bus = dbus.SessionBus()
    loop = GLib.MainLoop()

    manager = VlcGestureManager()
    manager.bus = bus

    def on_name_owner_changed(name, old_owner, new_owner):
        if name == 'org.mpris.MediaPlayer2.vlc':
            if new_owner:
                manager.start_worker()
            else:
                manager.stop_worker()

    bus.add_signal_receiver(
        on_name_owner_changed,
        'NameOwnerChanged',
        'org.freedesktop.DBus',
        'org.freedesktop.DBus',
        '/org/freedesktop/DBus'
    )

    if 'org.mpris.MediaPlayer2.vlc' in bus.list_names():
        manager.start_worker()

    def handle_sig(sig, frame):
        manager.stop_worker()
        loop.quit()

    signal.signal(signal.SIGTERM, handle_sig)
    signal.signal(signal.SIGINT, handle_sig)

    loop.run()

if __name__ == '__main__':
    main()
