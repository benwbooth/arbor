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
  };

  config = lib.mkIf cfg.enable {
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
      };

      # Screencasting goes through arbor's Mutter-compatible D-Bus API, which
      # the GNOME portal speaks.
      extraPortals = [
        pkgs.xdg-desktop-portal-gnome
        pkgs.xdg-desktop-portal-gtk
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
  };
}
