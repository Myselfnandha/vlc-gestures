#!/usr/bin/python3 -O
import os
import sys
import glob
import time
import select
import threading
import signal
import fcntl
import dbus
from dbus.mainloop.glib import DBusGMainLoop
from gi.repository import GLib
from Xlib import X, display, XK, error as xerr
from Xlib.ext import xtest

LOCK_FILE = '/tmp/vlc-hold-speed.lock'
lock_fd = None


def acquire_lock():
    global lock_fd
    try:
        lock_fd = open(LOCK_FILE, 'a+')
        fcntl.flock(lock_fd.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        return True
    except (IOError, BlockingIOError, OSError):
        if lock_fd is not None:
            try:
                lock_fd.close()
            except Exception:
                pass
            lock_fd = None
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
    """Event-driven VLC click/hold-speed manager (0% CPU when unfocused via X11 PropertyNotify + select)."""

    def __init__(self):
        self.stop_event = threading.Event()
        self.worker_thread = None
        self.bus = None
        self._wake_w = None

    def _wake(self):
        if self._wake_w is not None:
            try:
                os.write(self._wake_w, b"x")
            except OSError:
                pass

    def start_worker(self):
        if self.worker_thread and self.worker_thread.is_alive():
            return
        self.stop_event.clear()
        self.worker_thread = threading.Thread(target=self._run_loop, daemon=True)
        self.worker_thread.start()

    def stop_worker(self):
        self.stop_event.set()
        self._wake()
        if self.worker_thread and self.worker_thread.is_alive():
            self.worker_thread.join(timeout=1.5)
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
                self.stop_event.wait(0.3)

        if d is None or self.stop_event.is_set():
            return

        wake_r, wake_w = os.pipe()
        os.set_blocking(wake_r, False)
        self._wake_w = wake_w

        root = d.screen().root
        net_active = d.intern_atom('_NET_ACTIVE_WINDOW')
        state_fullscreen = d.intern_atom('_NET_WM_STATE_FULLSCREEN')
        wm_state = d.intern_atom('_NET_WM_STATE')
        space_code = d.keysym_to_keycode(XK.string_to_keysym('space'))

        # Event-driven watch on root window focus changes (eliminates 5 Hz/20 Hz _NET_ACTIVE_WINDOW polling)
        root.change_attributes(event_mask=X.PropertyChangeMask)
        d.flush()

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
        geo_dirty = True

        def get_player_interfaces():
            try:
                obj = bus.get_object('org.mpris.MediaPlayer2.vlc', '/org/mpris/MediaPlayer2')
                props = dbus.Interface(obj, 'org.freedesktop.DBus.Properties')
                player = dbus.Interface(obj, 'org.mpris.MediaPlayer2.Player')
                return props, player
            except Exception:
                return None, None

        def reset_rate_if_needed():
            nonlocal accelerated, is_pressed, pending_click
            if accelerated and player_props:
                try:
                    player_props.Set('org.mpris.MediaPlayer2.Player', 'Rate', dbus.Double(original_rate))
                except Exception:
                    pass
                accelerated = False
            is_pressed = False
            pending_click = False

        def refresh_active_window():
            nonlocal last_active_id, is_vlc, vlc_win, cached_geo, geo_dirty
            try:
                active_prop = root.get_full_property(net_active, X.AnyPropertyType)
            except Exception:
                active_prop = None

            if not active_prop or not active_prop.value or not active_prop.value[0]:
                last_active_id = None
                is_vlc = False
                vlc_win = None
                cached_geo = None
                reset_rate_if_needed()
                return

            active_id = active_prop.value[0]
            if active_id != last_active_id:
                last_active_id = active_id
                try:
                    win = d.create_resource_object('window', active_id)
                    wm_class = win.get_wm_class()
                    is_vlc = bool(wm_class and 'vlc' in [c.lower() for c in wm_class])
                    if is_vlc:
                        vlc_win = win
                        win.change_attributes(event_mask=X.StructureNotifyMask | X.PropertyChangeMask)
                        d.flush()
                        geo_dirty = True
                        cached_geo = None
                    else:
                        vlc_win = None
                        cached_geo = None
                        reset_rate_if_needed()
                except Exception:
                    is_vlc = False
                    vlc_win = None
                    cached_geo = None
                    reset_rate_if_needed()

        refresh_active_window()
        x_fd = d.fileno()

        try:
            while not self.stop_event.is_set():
                try:
                    # Drain queued X11 events cleanly before deciding sleep timeout
                    focus_dirty = False
                    while d.pending_events():
                        ev = d.next_event()
                        if ev.type == X.PropertyNotify and ev.window == root and ev.atom == net_active:
                            focus_dirty = True
                        elif vlc_win is not None and ev.window == vlc_win:
                            if ev.type == X.ConfigureNotify or (ev.type == X.PropertyNotify and ev.atom == wm_state):
                                geo_dirty = True
                            elif ev.type == X.DestroyNotify:
                                focus_dirty = True

                    if focus_dirty:
                        refresh_active_window()

                    if self.stop_event.is_set():
                        break

                    if not is_vlc or vlc_win is None:
                        # Sleep completely inside kernel select() until focus returns to VLC or shutdown
                        d.flush()
                        readable, _, _ = select.select([x_fd, wake_r], [], [])
                        if wake_r in readable:
                            try:
                                os.read(wake_r, 64)
                            except OSError:
                                pass
                        continue

                    now = time.time()

                    if pending_click and not is_pressed and (now - pending_click_time >= 0.30):
                        try:
                            xtest.fake_input(d, X.KeyPress, space_code)
                            d.sync()
                            xtest.fake_input(d, X.KeyRelease, space_code)
                            d.sync()
                        except Exception:
                            _, player = get_player_interfaces()
                            if player:
                                try:
                                    player.PlayPause()
                                except Exception:
                                    pass
                        pending_click = False

                    if geo_dirty or cached_geo is None:
                        try:
                            cached_geo = vlc_win.get_geometry()
                            state_prop = vlc_win.get_full_property(wm_state, X.AnyPropertyType)
                            is_fullscreen = bool(state_prop and state_fullscreen in state_prop.value)
                            geo_dirty = False
                        except (xerr.BadWindow, Exception):
                            cached_geo = None
                            refresh_active_window()
                            continue

                    try:
                        q = vlc_win.query_pointer()
                    except (xerr.BadWindow, Exception):
                        refresh_active_window()
                        continue

                    button1_down = bool(q.mask & X.Button1Mask)
                    in_video_area = False
                    if 0 <= q.win_x < cached_geo.width:
                        if is_fullscreen:
                            in_video_area = (q.win_y < cached_geo.height - 60)
                        else:
                            in_video_area = (40 <= q.win_y < (cached_geo.height - 85))

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
                        elif not accelerated and (now - press_start_time >= 0.35):
                            pending_click = False
                            props, _ = get_player_interfaces()
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
                            elif now - press_start_time < 0.35:
                                pending_click = True
                                pending_click_time = now
                            is_pressed = False

                    if button1_down:
                        timeout = 0.025
                    elif pending_click:
                        timeout = max(0.01, min(0.04, 0.30 - (now - pending_click_time)))
                    else:
                        timeout = 0.065

                    d.flush()
                    readable, _, _ = select.select([x_fd, wake_r], [], [], timeout)
                    if wake_r in readable:
                        try:
                            os.read(wake_r, 64)
                        except OSError:
                            pass

                except Exception:
                    self.stop_event.wait(0.1)
        finally:
            reset_rate_if_needed()
            self._wake_w = None
            for fd in (wake_r, wake_w):
                try:
                    os.close(fd)
                except OSError:
                    pass
            try:
                d.close()
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
