Notideck @VERSION@ (Linux, x86_64)
==================================
Notideck turns an old Android phone into a Stream Deck-style controller.
This folder is the tarball; the same files are in the AppImage.

Install
  ./install.sh                  per-user, into ~/.local (no root needed)
  ./install.sh --system         into /usr/local (uses sudo)
  ./install.sh --prefix DIR     anywhere else
  Options: --yes (no questions), --with-udev, --with-service, --with-webcam.
  Remove it again with <prefix>/share/notideck/uninstall.sh (add --purge to also delete your settings).

First run
  1. Install the Notideck app on the phone.
  2. Start "Notideck" from your app launcher, or run: notideck --open
     This starts the background agent and opens the config page in your browser.
     Starting it again just re-opens the page.
  3. Plug the phone in with USB and accept the "Allow USB debugging?" prompt
     (Settings > About phone > tap Build number 7 times > Developer options > USB debugging).
  adb (Google's Android platform-tools, see share/notideck/adb-NOTICE.txt) is bundled.

Keyboard access (hotkeys and typing)
  Needs /dev/uinput. Easiest: Notideck > Settings > Keyboard access > "Fix keyboard access".
  Or run ./install.sh --with-udev, then log out and back in.

Webcam (v4l2loopback)
  Needs the v4l2loopback kernel module (Arch/CachyOS/Debian/Ubuntu: v4l2loopback-dkms; Fedora: v4l2loopback
  from RPM Fusion). Easiest: Notideck > Camera > "Set up webcam" (asks for your password).
  Or ./install.sh --with-webcam, or: sudo notideck --install-vcam. uninstall.sh removes it again.

Start at login
  ./install.sh --with-service installs and enables the systemd user service
  (journalctl --user -u notideck-agent -f).

Plugins
  Bundled plugins live in <prefix>/lib/notideck/plugins. Your own go in ~/.config/notideck/plugins/<id>/.

Settings and logs: ~/.config/notideck. Licence: GPL-3.0-or-later (see LICENSE).

Window and tray
   Notideck opens its own window and a tray icon. This needs the system libraries webkit2gtk-4.1,
   gtk3 and libayatana-appindicator (Debian/Ubuntu: libwebkit2gtk-4.1-0 libgtk-3-0
   libayatana-appindicator3-1; Arch: webkit2gtk-4.1 gtk3 libayatana-appindicator) and a tray host
   (GNOME: the AppIndicator extension). With --headless, or without a display, the agent runs
   without a window and the config page opens in the browser.
