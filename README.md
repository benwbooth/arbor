# arbor

A Wayland compositor with a **nested docking layout under a zoomable camera**.

- New windows float on the Desktop; drag one by its title bar onto the edge
  of another window (or of the screen) to dock it beside it, or onto its
  middle to make a tab group. Dividers resize just the two windows they sit
  between.
- The dock tree is laid out on a canvas that can grow past the screen; a
  per-workspace camera follows the focused window, frames any part of the
  tree, or zooms out to show everything (touchpad pinch, keys, IPC). Windows
  too small to read become app-icon tiles.
- A Mission Control-style overview, macOS-style title bars for server-side
  decorated apps, the KDE appmenu protocol for a global menu bar, per-output
  EDID colour correction and HDR for SDR content.

See [ARBOR.md](ARBOR.md) for the design, the full feature list, a config
and actions guide, and a map of the code.

## Names

| What | Name |
| --- | --- |
| Binary | `arbor` (`arbor msg ...`, `arbor validate`, `arbor --session`, `arbor completions`) |
| Session | `arbor-session`, `arbor.service`, `arbor-shutdown.target`, `wayland-sessions/arbor.desktop` (`XDG_CURRENT_DESKTOP=arbor`) |
| Config | `$XDG_CONFIG_HOME/arbor/config.kdl` (or `$ARBOR_CONFIG`; system-wide `/etc/arbor/config.kdl`) |
| IPC socket | `$XDG_RUNTIME_DIR/arbor.<wayland-display>.<pid>.sock`, in `$ARBOR_SOCKET` (and in `$NIRI_SOCKET`, for bars and shells written for niri's IPC) |
| Portal config | `xdg-desktop-portal/arbor-portals.conf` |

## Installing on NixOS

```nix
{
  inputs.arbor.url = "github:benwbooth/arbor";

  outputs = { nixpkgs, arbor, ... }: {
    nixosConfigurations.my-machine = nixpkgs.lib.nixosSystem {
      modules = [
        arbor.nixosModules.default
        { programs.arbor.enable = true; }
      ];
    };
  };
}
```

`programs.arbor` installs the package, the session (pick "arbor" in your
display manager), the user units, xdg-desktop-portal (gnome + gtk by
default; override `xdg.portal.config.arbor`), polkit and dconf. It does not
enable GNOME Keyring. The package alone is `packages.<system>.arbor`
(`default`).

## Building

```sh
nix develop -c cargo build      # target/debug/arbor
nix build .#arbor
```

## Credits

arbor is derived from [niri](https://github.com/YaLTeR/niri) by Ivan
Molodetskikh and contributors (forked at v26.04; it is now developed
independently and no longer tracks upstream). It keeps niri's IPC protocol
and most of its config format, and the copyright notices of the code it
inherited. Licensed GPL-3.0-or-later, like niri; see [LICENSE](LICENSE).
