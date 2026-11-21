# arbor-rdp: RDP server for the arbor session.
#
# This is hypr-rdp by MuNeNICK (https://github.com/MuNeNICK/hypr-rdp, MIT;
# IronRDP, H.264 via VA-API with a bundled OpenH264 fallback, PipeWire audio,
# clipboard incl. files, PAM auth) with ./arbor-rdp.patch on top: without
# HYPRLAND_INSTANCE_SIGNATURE it skips Hyprland IPC, reads outputs from
# wl_output + xdg-output, and refuses the headless virtual-screen mode with
# a clear error (mirror a physical output with `output = "DP-1"` instead).
# Capture, input and clipboard already use standard protocols arbor speaks
# (wlr-screencopy, virtual keyboard/pointer, data-control). The binary,
# default config dir (~/.config/arbor-rdp) and default PAM service are renamed
# to arbor-rdp.
{
  lib,
  rustPlatform,
  fetchFromGitHub,
  pkg-config,
  cmake,
  clang,
  makeWrapper,
  libdrm,
  libgbm,
  libva,
  libxkbcommon,
  mesa,
  pipewire,
  pam,
  pulseaudio,
  wayland,
}:

rustPlatform.buildRustPackage rec {
  pname = "arbor-rdp";
  version = "0.1.8";

  src = fetchFromGitHub {
    owner = "MuNeNICK";
    repo = "hypr-rdp";
    rev = "v${version}";
    hash = "sha256-s3kQqnn1Ljg2tp/r5zohvrFbv2SUkxq28UMnMr+xosk=";
  };

  patches = [ ./arbor-rdp.patch ];

  cargoHash = "sha256-QIacYWd/hzVYv/bbqj1Vo76prTl2/xJGEd1CuwgEWhw=";

  nativeBuildInputs = [
    pkg-config
    cmake
    clang
    makeWrapper
    rustPlatform.bindgenHook
  ];

  buildInputs = [
    libdrm
    libgbm
    libva
    libxkbcommon
    mesa
    pipewire
    pam
    wayland
  ];

  # pactl for the default `redirect` audio mode; the setuid fusermount3
  # wrapper for receiving clipboard files from the client.
  postInstall = ''
    mv $out/bin/hypr-rdp $out/bin/arbor-rdp
    install -Dm644 LICENSE $out/share/licenses/arbor-rdp/LICENSE
    wrapProgram $out/bin/arbor-rdp \
      --run 'if test -z "''${FUSERMOUNT_PATH-}" && test -x /run/wrappers/bin/fusermount3; then export FUSERMOUNT_PATH=/run/wrappers/bin/fusermount3; fi' \
      --prefix PATH : ${lib.makeBinPath [ pulseaudio ]}
  '';

  # Upstream's suite spins up Wayland/RDP fixtures and PAM helper processes
  # and skips it in its own package too. To run it: apply the patch to a
  # checkout and `nix develop .#arbor-rdp -c cargo test` there.
  doCheck = false;

  meta = {
    description = "RDP server for the arbor session (hypr-rdp with arbor support)";
    homepage = "https://github.com/MuNeNICK/hypr-rdp";
    license = lib.licenses.mit;
    mainProgram = "arbor-rdp";
    platforms = lib.platforms.linux;
  };
}
