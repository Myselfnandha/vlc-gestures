# vlc-gestures

Touchscreen and mouse gestures for VLC media player on Linux (X11).

## Features
- **Hold for 1.5x Speed:** Press and hold anywhere on the video area (mouse left-click or touchscreen) to accelerate playback to 1.5x. Releasing instantly restores normal 1.0x playback speed.
- **Single Tap / Click to Play & Pause:** Tapping or clicking the video toggles playback and renders the native VLC On-Screen Display (OSD) symbol (`▶` or `❚❚`).
- **Double Tap / Click for Fullscreen:** Smoothly toggles fullscreen without pausing or interrupting video playback.
- **Pure Event-Driven:** Waits on D-Bus signals for VLC lifecycle events. Consumes 0.0% CPU when VLC is closed.
- **Pass-through Controls:** Clicks on bottom seekbars, volume sliders, and top window titlebars are passed directly to VLC.

## Installation

```bash
git clone https://github.com/Myselfnandha/vlc-gestures.git
cd vlc-gestures
./install.sh
```

## Dependencies
- `python3`
- `python-xlib`
- `python-dbus`
- `xdotool`
- `vlc`

On Arch Linux:
```bash
sudo pacman -S python-xlib python-dbus xdotool vlc
```

## License
MIT
