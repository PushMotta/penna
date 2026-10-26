# Penna for Omarchy

The shell side of [Penna](https://github.com/PushMotta/penna), fan control for Linux made to be understood: a glyph in the bar,
a popup, and a studio for curves and setup. Every fan is named for what it cools, with its RPM beside what it was asked for,
and anything that goes wrong is said in a sentence.

![The studio: the CPU fan's curve dragged and saved, with its stall line, its RPM scale and five minutes of every sensor](https://raw.githubusercontent.com/PushMotta/penna/92c88356b6ff2871eb01ff10422060fcf9846d82/docs/media/studio.gif)

<img src="https://raw.githubusercontent.com/PushMotta/penna/92c88356b6ff2871eb01ff10422060fcf9846d82/docs/media/popup.gif" alt="The popup switching profiles, then Full speed on and off" width="397" align="right">

The **popup** is for every day: the profiles, the temperatures the curves follow, and every fan with its role, RPM and duty.
*Defaults* hands every fan back to its BIOS or driver; *Full speed* puts them all at 100 % until you turn it off.

The **studio** is for curves: one card per fan, the editor with the fan's own stall line and RPM scale, and *Set up fans…*,
which measures every fan and proposes Quiet, Balanced and Render curves. Penna's README shows
[every part](https://github.com/PushMotta/penna#what-it-looks-like), setup and the themes included.
<br clear="right">

The plugin draws; it changes nothing by itself. The fans are run by `pennad`, Penna's daemon, which the `penna` package
installs as a system service. Without it the glyph says that Penna isn't running, and the fans stay with their firmware.

## Install

The daemon first. This version of the plugin names one release of Penna, the one it was reviewed with: **Penna 0.10.6**,
built from commit `92c88356b6ff2871eb01ff10422060fcf9846d82` (tag `v0.10.6`). Its package is
`penna-0.10.6-1-x86_64.pkg.tar.zst`, with the SHA-256 digest
`d366be6f378aacdf3ce0d74e065252f08d90399f633d21cc28f383bcb0c789f8`. The second line checks the download against that
digest, written here, rather than against the release's own `SHA256SUMS`, which is published beside the file it checks. Each
line runs only if the one before it worked, so a download that doesn't match is never installed:

```
curl -fLO https://github.com/PushMotta/penna/releases/download/v0.10.6/penna-0.10.6-1-x86_64.pkg.tar.zst &&
echo "d366be6f378aacdf3ce0d74e065252f08d90399f633d21cc28f383bcb0c789f8  penna-0.10.6-1-x86_64.pkg.tar.zst" | sha256sum -c &&
sudo pacman -U penna-0.10.6-1-x86_64.pkg.tar.zst
```

Or build the package from that commit yourself, which runs every test first:

```
git clone https://github.com/PushMotta/penna && git -C penna checkout --detach 92c88356b6ff2871eb01ff10422060fcf9846d82 && cd penna/packaging && makepkg -si
```

Then the service, the group and the plugin, again each only if the one before it worked:

```
sudo systemctl enable --now pennad &&         # the first start adopts nothing: every fan stays with its firmware
sudo usermod -aG penna "$USER" &&             # change fans without root; applies at once
omarchy plugin add https://github.com/PushMotta/omarchy-penna.git --enable
```

`omarchy plugin add` clones this repository into `~/.config/omarchy/plugins/io.github.pushmotta.penna`, validates the
manifest and enables the plugin, and with `--enable` the shell loads it at once. No script from the repository runs, and the
plugin never asks for root.

The plugin works with any `pennad` from 0.9.0 on, and says which to update when the two don't fit. A later version of the
plugin that names a later release of Penna gives its commit and digest here in the same way. Update to it with
`omarchy plugin update io.github.pushmotta.penna`.

Then click the glyph and choose *Set up fans…*. Setup measures every fan in about five minutes and proposes Quiet, Balanced and
Render curves. Nothing is kept until you press *Save*.

## What it shows

- **The glyph** carries the state: the fans running normally, a fan that needs a look, an emergency, setup running. A
  temperature beside it is a widget setting.
- **The popup**: profiles as pills, with *Defaults* first, which hands every fan back to its BIOS or driver. Then the
  temperatures, every fan with its RPM and duty, *Full speed*, and *Curves…*.
- **The studio**: one card per fan with its kind and support tier, the curve editor, and setup. `penna open` opens it from a
  keybinding.

Every colour, size and spacing comes from the shell's theme tokens, and motion follows Hyprland's `animations:enabled`.

## What it touches

The plugin connects to `pennad`'s socket: `/run/penna/penna.sock`, then `$XDG_RUNTIME_DIR/penna/penna.sock`, where a mock
desk listens. Only root can write the first and only you the second. `$PENNA_SOCKET` replaces both; point it only into a
directory no one else can write. It runs `hyprctl getoption animations:enabled`, writes no files, starts no services and runs
nothing as root.

The names it shows, of fans, sensors, curves and profiles, come from `pennad`, and any member of the `penna` group can set them
for every user. They are always drawn as text, never as markup.

## Removing it

```
omarchy plugin remove io.github.pushmotta.penna
```

To remove the daemon too, remove its package. That stops `pennad` first, which hands every fan back:

```
sudo pacman -R penna
```

## Developing

This repository is generated from `plugin/` in [PushMotta/penna](https://github.com/PushMotta/penna), and each version is
tied to the release of Penna its README names. Change it there; its CONTRIBUTING.md says how to run it against a simulated
desk.

## License

MIT. See [LICENSE](LICENSE).
