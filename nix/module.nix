# NixOS module for the arbor session, modelled on nixpkgs'
# nixos/modules/programs/wayland/niri.nix (with its wayland-session.nix
# inlined). Unlike that module it does not turn on GNOME Keyring.
self:
{
  config,
  options,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.programs.arbor;
  rdp = cfg.rdp;
  rdpConfig = (pkgs.formats.toml { }).generate "arbor-rdp.toml" rdp.settings;
  # Shipped as a package unit (with [Install]) rather than a NixOS
  # `systemd.user.services` entry, so it stays off until enabled at runtime
  # (`systemctl --user enable --now arbor-rdp`, or hearth's tray switch).
  rdpUnit = pkgs.writeTextDir "lib/systemd/user/arbor-rdp.service" ''
    [Unit]
    Description=arbor-rdp: RDP server for the arbor session
    Documentation=https://github.com/benwbooth/arbor
    PartOf=graphical-session.target
    After=graphical-session.target pipewire.service wireplumber.service
    # Only inside arbor: under Plasma, KRDP owns port 3389.
    ConditionEnvironment=ARBOR_SOCKET

    [Service]
    ExecStart=${lib.getExe rdp.package} --config ${rdpConfig}
    Restart=on-failure
    RestartSec=3

    [Install]
    WantedBy=graphical-session.target
  '';
  # `enableDefaultPath` is fairly new in nixpkgs; only set it where it exists.
  hasEnableDefaultPath =
    (options.systemd.user.services.type.nestedTypes.elemType.getSubOptions [ ]) ? enableDefaultPath;
in
{
  options.programs.arbor = {
    enable = lib.mkEnableOption "arbor, a Wayland compositor with a docking layout under a zoomable camera";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.arbor;
      defaultText = lib.literalExpression "arbor.packages.\${pkgs.stdenv.hostPlatform.system}.arbor";
      description = "The arbor package to use.";
    };

    useNautilus = lib.mkEnableOption "Nautilus as file-chooser for xdg-desktop-portal-gnome" // {
      default = true;
    };

    rdp = {
      enable = lib.mkEnableOption ''
        arbor-rdp, an RDP server for the arbor session (hypr-rdp with arbor
        support). This installs the `arbor-rdp.service` user unit and its PAM
        service; the unit is not started automatically: enable it at runtime
        with `systemctl --user enable --now arbor-rdp` (or hearth's tray).
        It only runs inside arbor (ConditionEnvironment=ARBOR_SOCKET). The
        firewall is left alone: open TCP 3389 yourself if needed
      '';

      package = lib.mkOption {
        type = lib.types.package;
        default = self.packages.${pkgs.stdenv.hostPlatform.system}.arbor-rdp;
        defaultText = lib.literalExpression "arbor.packages.\${pkgs.stdenv.hostPlatform.system}.arbor-rdp";
        description = "The arbor-rdp package to use.";
      };

      settings = lib.mkOption {
        description = ''
          arbor-rdp configuration, written to a TOML file passed with
          `--config` (keys as in hypr-rdp's config.toml, e.g. `resolution`,
          `bitrate`, `audio_mode`, `capture_mode`, `cert`, `key`).
        '';
        default = { };
        example = lib.literalExpression ''
          {
            bind = "0.0.0.0:3389";
            output = "DP-1";
            fps = 60;
          }
        '';
        type = lib.types.submodule {
          freeformType = (pkgs.formats.toml { }).type;
          options = {
            bind = lib.mkOption {
              type = lib.types.str;
              default = "127.0.0.1:3389";
              example = "0.0.0.0:3389";
              description = "Address and port to listen on. The default only accepts local connections.";
            };
            output = lib.mkOption {
              type = lib.types.str;
              example = "DP-1";
              description = ''
                Output (screen) to mirror. Required: arbor-rdp's virtual
                headless screen needs Hyprland and is unavailable on arbor.
                `arbor msg outputs` lists the names.
              '';
            };
            fps = lib.mkOption {
              type = lib.types.ints.positive;
              default = 30;
              description = "Maximum frame rate sent to the client.";
            };
            auth_mode = lib.mkOption {
              type = lib.types.enum [
                "pam"
                "configured"
              ];
              default = "pam";
              description = ''
                `pam`: log in with your system account password (PAM service
                `pam_service`). `configured`: a fixed `username` and
                `password_file` set here.
              '';
            };
            pam_service = lib.mkOption {
              type = lib.types.nullOr lib.types.str;
              default = if rdp.settings.auth_mode == "pam" then "arbor-rdp" else null;
              defaultText = lib.literalExpression ''if auth_mode == "pam" then "arbor-rdp" else null'';
              description = "PAM service used with `auth_mode = \"pam\"`; the module defines `arbor-rdp`.";
            };
          };
        };
      };
    };
  };

  config = lib.mkIf cfg.enable (
    lib.mkMerge [
      {
        environment.systemPackages = [ cfg.package ];

        # Required for xdg-desktop-portal-gnome's FileChooser to work properly.
        services.dbus.packages = lib.mkIf cfg.useNautilus [ pkgs.nautilus ];

        services.displayManager.sessionPackages = [ cfg.package ];

        # arbor.service and arbor-shutdown.target (lib/systemd/user).
        systemd.packages = [ cfg.package ];

        # Restarting the compositor kills the graphical session; same treatment as
        # the display-manager modules.
        systemd.user.services.arbor = {
          restartIfChanged = false;
        }
        // lib.optionalAttrs hasEnableDefaultPath {
          # Defining the unit here generates a drop-in; without this it would carry
          # the NixOS default Environment="PATH=coreutils:...", clobbering the PATH
          # that arbor-session imported into the user manager and breaking spawn
          # actions that rely on it.
          enableDefaultPath = false;
        };

        xdg.portal = {
          enable = lib.mkDefault true;

          # NOTE: `configPackages` is ignored when `xdg.portal.config.arbor` is
          # defined. No Secret portal here: GNOME Keyring is not enabled by this
          # module (set "org.freedesktop.impl.portal.Secret" yourself).
          config.arbor = {
            default = [
              "gnome"
              "gtk"
            ];
            "org.freedesktop.impl.portal.Access" = "gtk";
            "org.freedesktop.impl.portal.FileChooser" = lib.mkIf (!cfg.useNautilus) "gtk";
            "org.freedesktop.impl.portal.Notification" = "gtk";
            # arbor's own GlobalShortcuts backend (share/xdg-desktop-portal/portals/arbor.portal).
            "org.freedesktop.impl.portal.GlobalShortcuts" = "arbor";
          };

          # Screencasting goes through arbor's Mutter-compatible D-Bus API, which
          # the GNOME portal speaks.
          extraPortals = [
            pkgs.xdg-desktop-portal-gnome
            pkgs.xdg-desktop-portal-gtk
            # arbor.portal: the GlobalShortcuts backend arbor itself provides.
            cfg.package
          ];
        };

        security = {
          polkit.enable = true;
          pam.services.swaylock = { };
        };

        programs.dconf.enable = lib.mkDefault true;

        services.graphical-desktop.enable = true;

        # Window manager only sessions (unlike DEs) don't handle XDG autostart
        # files, so force them to run the service.
        services.xserver.desktopManager.runXdgAutostartIfNone = lib.mkDefault true;
      }

      (lib.mkIf rdp.enable {
        environment.systemPackages = [ rdp.package ];
        systemd.packages = [ rdpUnit ];

        # Password login over RDP: the default unix stack, no fingerprint (there
        # is no reader on the remote end; fprintd would just stall the login).
        security.pam.services.arbor-rdp = {
          fprintAuth = false;
        };

        # Receiving files from the client's clipboard mounts a FUSE tree.
        programs.fuse.enable = lib.mkDefault true;
      })
    ]
  );
}
