# Notideck user guide

Notideck turns an old Android phone into a Stream Deck-style controller and webcam for your PC. The phone shows a grid of keys, dials and sliders. You set them up in a config page on the PC. It works over a USB cable or over Wi-Fi.

## Contents

- [Requirements](#requirements)
- [Install on Windows](#install-on-windows)
- [Install on Linux](#install-on-linux)
- [First run](#first-run)
- [Using the deck](#using-the-deck)
- [Auto profile switching](#auto-profile-switching)
- [Volume mixer](#volume-mixer)
- [Media controls](#media-controls)
- [OBS](#obs)
- [Webcam](#webcam)
- [Microphone](#microphone)
- [Plugins](#plugins)
- [Settings](#settings)
- [Troubleshooting](#troubleshooting)
- [Updating](#updating)
- [Uninstalling](#uninstalling)

## Requirements

| | |
|---|---|
| Phone | Android 8.0 or newer. USB debugging is needed for USB mode and for installing the app from the PC. |
| PC | Windows 10 or 11 (64-bit), or Linux x86_64. |
| Webcam (optional) | Windows 11, or Linux with the v4l2loopback kernel module. Not on Windows 10. |
| Microphone (optional) | Linux: nothing extra. Windows: the free [VB-CABLE](https://vb-audio.com/Cable/), only if you want this feature. |

Optional extras, each needed only for its own feature:

| Extra | For | How |
|---|---|---|
| [VB-CABLE](https://vb-audio.com/Cable/) (Windows) | Phone as a microphone | See [Microphone](#microphone). |
| Webcam driver (Windows 11) | Phone as a webcam | Camera tab > **Install webcam (needs admin once)**. |
| `v4l2loopback` (Linux) | Phone as a webcam | Install `v4l2loopback-dkms` (Fedora: `v4l2loopback` from RPM Fusion), then see [Webcam](#webcam). |
| Keyboard access (Linux) | Hotkey and type-text keys | See [Keyboard access](#keyboard-access). |
| [OBS Studio](https://obsproject.com/) 28+ | OBS keys and templates | See [OBS](#obs). |
| AppIndicator extension (GNOME) | Tray icon | From GNOME Extensions. Other desktops have a tray already. |
| Wi-Fi mode | Phone and PC on the same network. |

## Install on Windows

### Setup program (recommended)

1. Run `Notideck-<version>-windows-x64-setup.exe`.
2. Accept the administrator prompt. The installer puts Notideck in `C:\Program Files\Notideck`.
3. Optionally tick the desktop shortcut.
4. On Windows 11 the installer also registers the webcam driver. If that fails, you get a warning and everything else still works. You can retry later from the Camera tab.
5. Tick "Run Notideck" on the last page, or open Notideck from the Start menu.

The setup program also adds Notideck to Settings > Apps, so you can uninstall it there.

### Portable zip

1. Unzip `Notideck-<version>-windows-x64.zip` anywhere.
2. Run `Notideck.exe`.

Nothing is installed. The zip has the same files as the setup program. Google's `adb.exe` is included.

Notideck opens a window and puts an icon in the system tray. Closing the window keeps Notideck running in the tray. To quit, use the tray menu.

## Install on Linux

You need an x86_64 desktop. Notideck opens its own window and a tray icon. This needs WebKitGTK 4.1, GTK 3 and libayatana-appindicator from your distribution (Debian/Ubuntu: `libwebkit2gtk-4.1-0 libgtk-3-0 libayatana-appindicator3-1`; Arch: `webkit2gtk-4.1 gtk3 libayatana-appindicator`) and a tray host (GNOME needs the AppIndicator extension). Without a display, or with `--headless`, it runs without a window and opens the config page in your browser.

### AppImage

```sh
chmod +x Notideck-<version>-x86_64.AppImage
./Notideck-<version>-x86_64.AppImage
```

This starts Notideck and opens the config page. `adb` and the bundled plugins are inside the image. If your system has no FUSE, run it with `--appimage-extract-and-run`.

### Tarball and install.sh

```sh
tar xzf Notideck-<version>-linux-x86_64.tar.gz
cd Notideck-<version>-linux-x86_64
./install.sh
```

The default is a per-user install into `~/.local`. Open "Notideck" from your app menu afterwards.

| Flag | Effect |
|---|---|
| `--prefix DIR` | Install into `DIR` instead of `~/.local`. |
| `--system` | Install into `/usr/local` (uses `sudo`). |
| `--yes` | Do not ask questions. Optional parts stay off unless you also pass their flag. |
| `--with-udev` | Install the udev rule for keyboard access (see below). |
| `--with-service` | Install a systemd user service that starts Notideck at login. |

Without `--yes`, the script asks about the udev rule and the service.

### Keyboard access

Keys that press shortcuts or type text need access to `/dev/uinput`. You have three ways to get it:

- In the config page, open **Settings**. Next to "Keyboard access", press **Fix...**. Enter your password when asked.
- Run `./install.sh --with-udev`.
- Do it by hand:

  ```sh
  sudo cp 70-notideck-uinput.rules /etc/udev/rules.d/
  sudo udevadm control --reload && sudo udevadm trigger
  sudo usermod -aG input "$USER"      # then log out and back in
  ```

If `/dev/uinput` does not exist, run `sudo modprobe uinput`. Without access, hotkey and typing keys do nothing.

USB mode on Linux needs `adb`. The AppImage and the tarball both include it.

## First run

On first start Notideck shows a setup wizard. **Skip setup** is on every step, and you can run it again from Settings > **Run setup again**.

| Step | What it does |
|---|---|
| Welcome | Short introduction. Press **Get started**. |
| Install the phone app | Lists phones connected over USB and installs the app. |
| Connect your phone | Shows the connection state, approves a phone, or pairs over Wi-Fi. |
| Pick a first profile | Pick a ready-made profile or **Start empty**. |
| A few extras | Webcam driver (Windows), keyboard access (Linux), **Start at login**. |
| You are all set | Press **Finish**. |

### Turn on USB debugging

Do this once on the phone.

1. Open **Settings > About phone** and tap **Build number** seven times.
2. Go back, open **Developer options** and turn on **USB debugging**.
3. Plug the phone into the PC.
4. Accept the "Allow USB debugging?" prompt on the phone. Tick "Always allow" so it does not ask again.

Menu names differ a little between phone makers.

### Install the phone app over USB

In the wizard step "Install the phone app", each phone found over USB is listed. Press **Install app** next to your phone. When it is done the row says "Installed v..." with the version.

The PC needs the phone app file (`notideck-phone.apk`). The Windows setup program and the zip include it when it was available at build time. If the wizard says no APK is bundled, get `Notideck-<version>-phone.apk` from the release and install it yourself, for example with `adb install -r`.

### Connect over USB

Open the Notideck app on the phone. Over USB it connects by itself. If the PC has not seen this phone before, an approval appears in the wizard and on the **Devices** tab. Press **Allow**. **Deny** refuses the phone.

If you do not want to use a phone yet, press **Continue without a phone**.

### Connect over Wi-Fi

1. In the wizard press **Pair over Wi-Fi**, or open **Devices** and press **Pair a phone**. A QR code appears. It expires after a short time. Press **New code** for a fresh one.
2. On the phone, open Notideck, go to **PCs** and tap **Scan QR**. Or point the phone's camera app at the code.
3. Keep the phone on the same Wi-Fi as the PC.

You pair once. After that the phone reconnects by itself. When both USB and Wi-Fi are available, USB is used.

If **Pair a phone** is greyed out, Wi-Fi connections are off. Turn on **Allow Wi-Fi connections** in Settings. This takes effect after Notideck restarts.

To remove a paired phone, press **Remove** on the Devices tab.

## Using the deck

The config page has these tabs: **Deck**, **Devices**, **Camera**, **Mixer**, **OBS**, **Auto-switch**, **Plugins**, **Settings**.

### Edit keys

On the **Deck** tab you see a preview of the phone screen. Pick a key to open its inspector, where you set its label, icon and action. Changes are saved as you make them ("Saved" shows at the top).

Keys can do these things, among others:

- press a hotkey or type text
- open a URL, launch an app (pick one from the installed apps list) or run a command
- control volume, media playback or OBS
- run an action from a plugin

Besides keys there are dials and sliders for values such as volume.

### Pages

A profile can have several pages. The page tabs are above the preview. Folders let a key open a sub-page.

### Profiles and templates

A profile is one complete deck. Use the toolbar at the top of the Deck tab:

- **Profile** menu: switch the profile you are editing. A profile marked "live" is the one on the phone.
- **New profile**: choose a name and a template.
- **Push to phone** (or **Re-push to phone**): make the profile you are editing the one on the phone.

Templates available in **New profile**:

| Group | Templates |
|---|---|
| (none) | Blank |
| Streaming | OBS Studio |
| Media | Media controls, Spotify |
| Apps | VS Code, Discord, TeamSpeak, Photoshop, GIMP, Premiere Pro, DaVinci Resolve |

The new profile dialog can also add an auto-switch rule for the template's app.

### On the phone

- **Side rail**: a thin bar on the edge of the phone screen. **Home** opens the main menu (your profiles). **Back** goes up one level. Page dots show where you are. The rail can also hold up to three keys that stay on screen on every page.
- **Swipe**: swipe left or right on the deck to move between pages.
- With auto-switching running, a long press on **Home** opens the main menu and pauses switching. A tap on the rail while paused resumes it.

## Auto profile switching

The **Auto-switch** tab changes the profile on the phone to match the app you are using. Windows works fully. Linux works on KDE Plasma (Wayland), Sway, Hyprland and X11; GNOME on Wayland is not supported yet.

1. Turn on **Auto-switch**.
2. Press **Add rule** (or pick an app from the recent apps list). Choose the app and the profile to show.
3. Press **Save**.

The first matching rule wins. Change the order with the up and down arrows. Other settings on this tab:

- **Lock the current profile** pauses switching until you unlock it.
- **Main profile** is what the "Main menu" key shows.
- **When no rule matches** picks the profile for other apps, or "Stay on current profile".

"Focused now" shows which app Notideck sees right now.

## Volume mixer

The **Mixer** tab controls the volume of single apps from keys, dials or sliders on the phone. Apps that play sound fill the mixer slots in the order shown. Use the arrows to reorder and **Hide** to keep an app out. Hidden apps are listed separately and can be shown again. **Show system sounds** adds the operating system's own sounds as an app.

On Windows the mixer uses WASAPI and shows level meters. On Linux it uses `pactl` (PulseAudio or PipeWire) and has no level meters.

## Media controls

The **Media controls** template gives you play, pause, next, previous and volume keys, and a tile with the current track. A second page has shuffle, repeat, seek, plus and minus 10 seconds, and volume for the player and the PC. These work with the player that is active on the PC: Windows media sessions on Windows, MPRIS on Linux.

## OBS

Open the **OBS** tab to connect to OBS Studio over obs-websocket.

1. If OBS is installed and its server is off, press **Enable it for me**. Otherwise use **Manual settings** to enter the port and password.
2. Press **Save**. Use **Reconnect** if OBS was not running.
3. Use **New profile from OBS template** for a ready-made deck (scenes, record, stream, mic). **Add OBS page** and **Add OBS audio page** add pages to the profile you have open.

Keys show live state, for example a red record key while recording.

## Webcam

The phone camera can act as a webcam on Windows 11 and on Linux. Windows 10 is not supported.

1. Open the **Camera** tab.
   - Windows 11: press **Install webcam (needs admin once)** and allow the administrator prompt. If you used the setup program or the wizard, this may already be done. You can also install from the wizard step "A few extras".
   - Linux: install the v4l2loopback kernel module first (Arch / CachyOS / Debian / Ubuntu: `v4l2loopback-dkms`; Fedora: `v4l2loopback` from RPM Fusion). Then press **Set up webcam (asks for password)** and enter your password once. Notideck sets the module to load at boot.
2. In Zoom, Teams, Discord, OBS, your browser or the Windows Camera app, pick **Notideck Camera**.
3. The phone camera starts when an app opens the camera. It stops a few seconds after the app closes. Keep the phone connected.

On the Camera tab you can choose the lens, use the torch, set zoom and adjust the picture. A test view shows what apps will get. **Refresh picture** forces a new frame.

To remove the driver: Settings > **Webcam driver** > **Uninstall...**.

## Microphone

The phone microphone can be an input for Discord, OBS, Zoom and other apps. Open the **Microphone** tab.

- **Linux:** nothing to install. Turn the microphone on, allow microphone access on the phone, and pick **Notideck Microphone** as the input in your app.
- **Windows:** Windows can't add a microphone without a signed driver, so Notideck plays the phone sound into a free virtual audio cable. This is optional: install it only if you want the phone as a microphone.
  1. Download [VB-CABLE](https://vb-audio.com/Cable/) by VB-Audio and unzip it.
  2. Right-click `VBCABLE_Setup_x64.exe`, choose **Run as administrator** and press **Install Driver**. Restart Windows if the installer asks.
  3. Notideck finds the cable by itself. Turn the microphone on in the Microphone tab and allow microphone access on the phone.
  4. In your app pick **CABLE Output (VB-Audio Virtual Cable)** as the input. Discord: User Settings > Voice & Video > Input Device.

  With **Keep my default speaker and mic** on, Notideck stops Windows from switching your default speaker or mic to the cable.

The tab also has mute, gain, the output device and **Noise suppression and echo cancellation** (done on the phone).

## Plugins

Plugins add new key actions. Open the **Plugins** tab to see, enable, disable and restart them, and to read their logs with **Show log**.

Bundled plugins:

| Plugin | What it does |
|---|---|
| Clock & Timer | Live clock, countdown timer, stopwatch keys. |
| HTTP request | Sends an HTTP request from a key and can show a value from the JSON reply. |
| Weather | Current temperature and conditions from Open-Meteo. |

Your own plugins go in the plugins folder inside the config folder:

- Windows: `%APPDATA%\Notideck\plugins`
- Linux: `~/.config/notideck/plugins`

The quickest way is **Install from .zip...** on the Plugins tab. **Open plugins folder** opens the folder, and **Reload** rescans it. A plugin you add must be approved first (**Approve...**). Plugins run with your permissions, so only install ones you trust.

To write a plugin, see [PLUGINS.md](PLUGINS.md).

## Settings

Open the **Settings** tab.

| Setting | What it does |
|---|---|
| Start at login | Opens Notideck when you sign in. |
| Start minimized to tray | Skips the window at start. |
| Allow Wi-Fi connections | Lets paired phones connect over the network. Needs a restart of Notideck. On by default. |
| Theme | System, Dark or Light for the config page. |
| Phone theme | Black (AMOLED), Graphite, Dusk, Tide (dark), or Cream, Lavender, Mint (light). Applies to the phone at once. |
| Phone screen | **Sleep when idle** (on by default) blacks out the deck and dims the backlight after the time in **After** (2 minutes by default); the first touch only wakes it. Optional wake when the PC uses the camera or mic, and a faint clock while asleep. |
| Keyboard access | Status of keyboard injection, with **Fix...** on Linux when it needs permission. |
| Run setup again | Reopens the setup wizard. |
| Webcam driver | Shows the driver state and lets you uninstall it. |
| Logs, Configuration | **Open logs folder** and **Open config folder**. |

## Troubleshooting

| Problem | What to do |
|---|---|
| Phone shows "unauthorized" in `adb devices` | Unlock the phone and look for the "Allow USB debugging?" prompt. Accept it. If it does not appear, unplug the cable, turn USB debugging off and on, and plug it in again. In Developer options, "Revoke USB debugging authorizations" resets old keys. |
| Phone does not show up on the PC | Use a data cable, not a charge-only one. Set the USB mode to File transfer. Check that USB debugging is on. Try another USB port. Only one adb server can run at a time, so close other tools that use adb (Android Studio, other phone managers). |
| Phone is listed but the app does not connect | Open the Notideck app on the phone. Look for a pending approval and press **Allow**. |
| Hotkeys and typing do nothing on Linux | Fix keyboard access, see [Keyboard access](#keyboard-access). |
| Notideck Camera is not in the camera list | You need Windows 11. Install the driver from the Camera tab. Restart the video app after installing. |
| Windows asks about the firewall when using Wi-Fi | Choose **Private networks** and allow. Without it, phones cannot reach Notideck over Wi-Fi. |
| Pairing code does not work | The code expires. Press **New code**. Check that both devices are on the same network. |
| Wi-Fi phone does not reconnect | Check that **Allow Wi-Fi connections** is on and that the PC and phone are on the same network. Try USB. |
| Something else is wrong | Open the logs and look for errors. |

### Logs and files

| | Windows | Linux |
|---|---|---|
| Logs | `%LOCALAPPDATA%\Notideck\logs` | `~/.local/state/notideck/logs` |
| Config (profiles, settings, paired phones, plugins) | `%APPDATA%\Notideck` | `~/.config/notideck` |

Logs are rotated daily and the last seven are kept. **Settings > Open logs folder** and **Open config folder** open them for you. To get more detail, start Notideck with the environment variable `RUST_LOG=debug`.

### Run setup again

Open **Settings** and press **Run setup again**.

### Command line

```
Notideck --help
```

Useful options: `--minimized` (start in the tray), `--open` (open the config page in the browser), `--quit` (ask a running Notideck to quit), `--console` (show log output, Windows). On Linux the program is called `notideck`, and `--headless` runs it without any window.

## Updating

- **Windows setup program**: run the new setup program. It closes the running Notideck, replaces the files and keeps your settings. The webcam driver is updated if it is installed.
- **Windows zip**: quit Notideck from the tray, then unzip the new version over the old folder.
- **Linux tarball**: extract the new tarball and run `./install.sh` again with the same flags.
- **AppImage**: download the new file and replace the old one.
- **Phone app**: install the new APK the same way as the first time (wizard step "Install the phone app", or `adb install -r`). Update the PC and the phone app together.

Profiles and pairing are kept.

## Uninstalling

### Windows

- **Setup program**: Settings > Apps > Notideck > Uninstall. It quits Notideck, removes the webcam driver and the files, and the "Start at login" entry. A box "Also remove my profiles, settings, paired phones and logs" deletes `%APPDATA%\Notideck` and `%LOCALAPPDATA%\Notideck`. Leave it unticked to keep them for a later reinstall.
- **Portable zip**: untick **Start at login** in Settings, quit Notideck from the tray, remove the webcam driver in Settings > Webcam driver > **Uninstall...** if you installed it, then delete the folder and the data folders above.

### Linux

- **Tarball install**: run `<prefix>/share/notideck/uninstall.sh` (for example `~/.local/share/notideck/uninstall.sh`). It removes everything `install.sh` put in place and keeps `~/.config/notideck`. Add `--purge` to delete your profiles, settings and paired phones too.
- **AppImage**: delete the file. Delete `~/.config/notideck` and `~/.local/state/notideck` if you want your settings gone.

On the phone, uninstall "Notideck" like any other app.
