# Penna for Omarchy

The shell side of [Penna](https://github.com/PushMotta/penna), fan control for Linux made to be understood: a glyph in the bar,
a popup, and a studio for curves and setup. Every fan is named for what it cools, with its RPM beside what it was asked for,
and anything that goes wrong is said in a sentence.

![The studio on Balanced: every fan named for its header, the CPU fan's generated curve with its stall line, and five minutes of every sensor](docs/media/studio.png)

<img src="docs/media/popup.png" alt="The popup: profiles, temperatures, and every fan with its role, RPM and duty" width="380">

The plugin draws; it changes nothing by itself. The fans are run by `pennad`, Penna's daemon, which the `penna` package
installs as a system service. Without it the glyph says that Penna isn't running, and the fans stay with their firmware.

## Install

The daemon first, from the AUR, then the plugin:

```
yay -S penna                                  # or paru -S penna
sudo systemctl enable --now pennad            # the first start adopts nothing: every fan stays with its firmware
sudo usermod -aG penna "$USER"                # change fans from the popup and the studio; applies at once
omarchy plugin add https://github.com/PushMotta/omarchy-penna.git --enable
```

`omarchy plugin add` clones this repository into `~/.config/omarchy/plugins/io.github.pushmotta.penna`, validates the
manifest and enables the plugin. Nothing from the repository runs during install, and the plugin never asks for root. Update
it with `omarchy plugin update io.github.pushmotta.penna` when you update the `penna` package: each release of one goes with
the same release of the other.

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

The plugin connects to `pennad`'s socket, `/run/penna/penna.sock` (or `$PENNA_SOCKET`), and runs
`hyprctl getoption animations:enabled`. It writes no files, starts no services and runs nothing as root.

## Removing it

```
omarchy plugin remove io.github.pushmotta.penna
sudo pacman -R penna                          # stops pennad first, which hands every fan back
```

## Developing

This repository is generated from `plugin/` in [PushMotta/penna](https://github.com/PushMotta/penna) at each release. Change
it there; its CONTRIBUTING.md says how to run it against a simulated desk.

## License

MIT. See [LICENSE](LICENSE).
