### Repository URL

https://github.com/PushMotta/omarchy-penna

### Category

Hardware

### Tags

Bar, Quickshell, System

### Suggest a missing tag

Fans

### Maintainer notes

Penna is fan control for Linux, made to be understood. This plugin is its shell side: a bar glyph that carries the fans'
state, a popup with profiles, temperatures and every fan's RPM, and a studio for curves and setup. Three kinds in one plugin:
bar-widget (popup), overlay (studio), service.

The fans are run by `pennad`, a system service from the separate `penna` package (MIT, https://github.com/PushMotta/penna).
The README installs exactly one release of it, the one this version was reviewed with, named by its full commit and its
package's SHA-256. The download is checked against the digest written in the README, or the package is built from that
commit, checked out detached, and each step runs only if the one before it worked. The README's `sudo` is in those install
commands (`pacman -U`, or `makepkg -si`, which calls it; `systemctl enable`; `usermod`) and in removing the package
(`pacman -R`), all run by the user; the plugin's code never installs, starts or elevates anything. The plugin depends on the
daemon and says so when it isn't running. The daemon drives fans only through the kernel's hwmon drivers and NVIDIA's NVML,
and it puts every fan back to its BIOS or driver on every way out, a crash and removing the package included.

What the plugin touches:
- It connects to `pennad`'s Unix socket, `/run/penna/penna.sock`, then `$XDG_RUNTIME_DIR/penna/penna.sock` for a mock desk
  (or `$PENNA_SOCKET` alone, when set). Only root can write the first and only the user the second. Only members of the
  `penna` group may change fans through it; anyone else gets a read-only view and a sentence saying how to join.
- It runs `hyprctl getoption animations:enabled -j` when it starts and after each Hyprland config reload, to follow
  Hyprland's motion preference.
- Every name it shows from `pennad` (fans, sensors, curves, profiles) is drawn as plain text, so a label another member of
  the `penna` group sets can't load or style anything on this user's desktop.
- Its code writes no files, starts no services, and runs no `sudo` or `pkexec`. When `pennad` isn't running, or the user can't
  change fans, it says so and points to `penna doctor`, the daemon package's command, which names what to do.

Nothing is downloaded at install or at runtime. The repository is generated from `plugin/` in the main repository, and each
version names the release of Penna it goes with. It holds the plugin, its tests (`node --test`, for Model.js), this text,
the README, the licence and `preview.png`; the README's screenshots and GIFs are linked from the main repository at a fixed
commit, so they aren't cloned. It holds no agent or session handoff material. The workflow's actions are pinned to full
commit SHAs, it declares `permissions: contents: read`, and its checkout keeps no credentials.

### Submission checklist

- [x] The repository is public and contains installation and removal instructions.
- [x] I have documented the plugin license and any external dependencies.
- [x] I confirm that I own or have permission to submit this plugin and its preview assets.
- [x] The plugin does not overwrite user configuration without explicit consent.
- [x] I understand that approval is for listing and is not a security review.
