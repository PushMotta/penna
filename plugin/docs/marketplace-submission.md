### Repository URL

https://github.com/PushMotta/omarchy-penna

### Category

Hardware

### Tags

Quickshell, System

### Suggest a missing tag

Fans

### Maintainer notes

Penna is fan control for Linux, made to be understood. This plugin is its shell side: a bar glyph that carries the fans'
state, a popup with profiles, temperatures and every fan's RPM, and a studio for curves and setup. Three kinds in one plugin:
bar-widget (popup), overlay (studio), service.

The fans are run by `pennad`, a system service from the separate `penna` package (AUR, MIT,
https://github.com/PushMotta/penna). The plugin depends on it and says so on the glyph when it isn't running. It never installs,
starts or elevates anything itself. The daemon drives fans only through the kernel's hwmon drivers and NVIDIA's NVML, and it
puts every fan back to its BIOS or driver on every way out, including a crash and removing the package.

What the plugin touches:
- It connects to `pennad`'s Unix socket, `/run/penna/penna.sock` (or `$PENNA_SOCKET`). Only members of the `penna` group may
  change fans through it; anyone else gets a read-only view and a sentence saying how to join.
- It runs `hyprctl getoption animations:enabled -j` once, to follow Hyprland's motion preference.
- It writes no files, starts no services, and contains no `sudo` or `pkexec`. Two messages tell the user a command to run
  themselves (`sudo systemctl start pennad`, `sudo usermod -aG penna $USER`).

Nothing is downloaded at install or at runtime. The repository is generated from `plugin/` in the main repository at each
release. It holds the plugin, its tests (`node --test`, for Model.js), this text, README and LICENSE. It holds no agent or
session handoff material. The workflow's actions are pinned to full commit SHAs, and it declares `permissions: contents: read`.

### Submission checklist

- [x] The repository is public and contains installation and removal instructions.
- [x] I have documented the plugin license and any external dependencies.
- [x] I confirm that I own or have permission to submit this plugin and its preview assets.
- [x] The plugin does not overwrite user configuration without explicit consent.
- [x] I understand that approval is for listing and is not a security review.
