{
  description = "arbor: a Wayland compositor with a docking layout under a zoomable camera.";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

    # NOTE: This is not necessary for end users
    # You can omit it with `inputs.rust-overlay.follows = ""`
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
    }:
    let
      revision = self.shortRev or self.dirtyShortRev or "unknown";
      arbor-package =
        {
          lib,
          cairo,
          dbus,
          libGL,
          libdisplay-info,
          libinput,
          seatd,
          libxkbcommon,
          libgbm,
          pango,
          pipewire,
          pkg-config,
          rustPlatform,
          systemd,
          wayland,
          installShellFiles,
          rsync,
          withDbus ? true,
          withSystemd ? true,
          withScreencastSupport ? true,
        }:

        let
          cargoDeps = rustPlatform.importCargoLock {
            # allowBuiltinFetchGit: smithay is a git dependency.
            allowBuiltinFetchGit = true;
            lockFile = ./Cargo.lock;
          };
          features =
            lib.optional withDbus "dbus"
            ++ lib.optional withScreencastSupport "xdp-gnome-screencast"
            ++ lib.optional withSystemd "systemd";
        in
        rustPlatform.buildRustPackage {
          pname = "arbor";
          version = revision;

          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              ./arbor-config
              ./arbor-ipc
              ./resources
              ./src
              ./Cargo.toml
              ./Cargo.lock
            ];
          };

          postPatch = ''
            patchShebangs resources/arbor-session
            substituteInPlace resources/arbor.service \
              --replace-fail 'ExecStart=arbor' "ExecStart=$out/bin/arbor"
          '';

          inherit cargoDeps;

          strictDeps = true;

          nativeBuildInputs = [
            rustPlatform.bindgenHook
            pkg-config
            installShellFiles
            rsync
          ];

          buildInputs =
            [
              cairo
              dbus
              libGL
              libdisplay-info
              libinput
              seatd
              libxkbcommon
              libgbm
              pango
              wayland
            ]
            ++ lib.optional (withDbus || withScreencastSupport || withSystemd) dbus
            ++ lib.optional withScreencastSupport pipewire
            # Also includes libudev
            ++ lib.optional withSystemd systemd;

          buildFeatures = features;
          buildNoDefaultFeatures = true;

          # Build in a persistent target directory (/var/tmp/cargo-nix/arbor,
          # exposed to the sandbox by Ben's system flake through
          # extra-sandbox-paths) so a new revision only recompiles what
          # changed instead of all of arbor and its dependencies. Without
          # that directory it builds in a throwaway one, as before.
          buildPhase = ''
            runHook preBuild
            workDir=$TMPDIR/cargo-nix
            if [ -w /var/tmp/cargo-nix ]; then
              workDir=/var/tmp/cargo-nix/arbor
            fi
            if [ "$cargoBuildType" = debug ]; then profile=dev; else profile=$cargoBuildType; fi
            # The store vendor copy only changes with Cargo.lock; the per-build
            # copy gets new mtimes, which would rebuild every dependency.
            sed -i "s|^directory = .*|directory = \"${cargoDeps}\"|" "$NIX_BUILD_TOP/.cargo/config.toml"
            (
              # Writable by everyone: each build may run as another nixbld
              # user. Only in this subshell, so the outputs stay normal.
              umask 000
              mkdir -p "$workDir/src" "$workDir/target"
              touch "$workDir"
              # Only rewrite changed files; cargo's freshness check uses mtimes.
              rsync -rc --no-times --no-perms --chmod=a+rwX --delete ./ "$workDir/src/"
              # The linker wrapper rejects output paths outside the build dir.
              export NIX_ENFORCE_PURITY=0
              cd "$workDir/src"
              CARGO_HOME="$NIX_BUILD_TOP/.cargo" CARGO_TARGET_DIR="$workDir/target" \
                cargo build --profile "$profile" --bin arbor --offline --locked \
                  -j "$NIX_BUILD_CORES" --no-default-features \
                  --features "${lib.concatStringsSep "," features}"
              find "$workDir/target" -mindepth 1 -user "$(id -u)" ! -perm -o+w \
                -exec chmod a+rwX {} + 2>/dev/null || true
            )
            mkdir -p target
            cp "$workDir/target/$cargoBuildType/arbor" target/arbor
            runHook postBuild
          '';

          installPhase = ''
            runHook preInstall
            install -Dm755 target/arbor -t $out/bin
            runHook postInstall
          '';

          # ever since this commit:
          # https://github.com/niri-wm/niri/commit/771ea1e81557ffe7af9cbdbec161601575b64d81
          # niri now runs an actual instance of the real compositor (with a mock backend) during tests
          # and thus creates a real socket file in the runtime dir.
          # this is fine for our build, we just need to make sure it has a directory to write to.
          preCheck = ''
            export XDG_RUNTIME_DIR="$(mktemp -d)"
          '';

          checkFlags = [
            # These tests require the ability to access a "valid EGL Display", but that won't work
            # inside the Nix sandbox
            "--skip=::egl"
          ];

          postInstall =
            ''
              installShellCompletion --cmd arbor \
                --bash <($out/bin/arbor completions bash) \
                --fish <($out/bin/arbor completions fish) \
                --nushell <($out/bin/arbor completions nushell) \
                --zsh <($out/bin/arbor completions zsh)

              install -Dm644 resources/arbor.desktop -t $out/share/wayland-sessions
              install -Dm644 resources/arbor-portals.conf -t $out/share/xdg-desktop-portal
              install -Dm644 resources/arbor.portal -t $out/share/xdg-desktop-portal/portals
            ''
            + lib.optionalString withSystemd ''
              install -Dm755 resources/arbor-session $out/bin/arbor-session
              # lib/systemd/user is where NixOS's systemd.packages looks (a
              # unit only under share/ left NixOS's drop-in as the whole
              # unit, with no ExecStart, and the session died at
              # login). nixpkgs' fixup moves it to share/ and links lib/ to it.
              install -Dm644 resources/arbor{.service,-shutdown.target} -t $out/lib/systemd/user
            '';

          env = {
            # Force linking with libEGL and libwayland-client
            # so they can be discovered by `dlopen()`
            RUSTFLAGS = toString (
              map (arg: "-C link-arg=" + arg) [
                "-Wl,--push-state,--no-as-needed"
                "-lEGL"
                "-lwayland-client"
                "-Wl,--pop-state"
              ]
            );
            ARBOR_BUILD_COMMIT = revision;
          };

          passthru = {
            providedSessions = [ "arbor" ];
          };

          meta = {
            description = "Wayland compositor with a docking layout under a zoomable camera";
            homepage = "https://github.com/benwbooth/arbor";
            license = lib.licenses.gpl3Only;
            mainProgram = "arbor";
            platforms = lib.platforms.linux;
          };
        };

      inherit (nixpkgs) lib;
      # Support all Linux systems that the nixpkgs flake exposes
      systems = lib.intersectLists lib.systems.flakeExposed lib.platforms.linux;

      forAllSystems = lib.genAttrs systems;
      nixpkgsFor = forAllSystems (system: nixpkgs.legacyPackages.${system});
    in
    {
      checks = forAllSystems (system: {
        # We use the debug build here to save a bit of time
        inherit (self.packages.${system}) arbor-debug;
      });

      devShells = forAllSystems (
        system:
        let
          pkgs = nixpkgsFor.${system};
          rust-bin = rust-overlay.lib.mkRustBin { } pkgs;
          inherit (self.packages.${system}) arbor;
        in
        {
          default = pkgs.mkShell {
            packages = [
              # We don't use the toolchain from nixpkgs
              # because we prefer a nightly toolchain
              # and we *require* a nightly rustfmt
              (rust-bin.selectLatestNightlyWith (
                toolchain:
                toolchain.default.override {
                  extensions = [
                    # includes already:
                    # rustc
                    # cargo
                    # rust-std
                    # rust-docs
                    # rustfmt-preview
                    # clippy-preview
                    "rust-analyzer"
                    "rust-src"
                  ];
                }
              ))
              pkgs.cargo-insta
            ];

            nativeBuildInputs = [
              pkgs.rustPlatform.bindgenHook
              pkgs.pkg-config
            ];

            buildInputs = arbor.buildInputs;

            env = {
              # WARN: Do not overwrite this variable in your shell!
              # It is required for `dlopen()` to work on some libraries; see the comment
              # in the package expression
              #
              # This should only be set with `CARGO_BUILD_RUSTFLAGS="$CARGO_BUILD_RUSTFLAGS -C your-flags"`
              CARGO_BUILD_RUSTFLAGS = arbor.RUSTFLAGS;
            };
          };
        }
      );

      formatter = forAllSystems (system: nixpkgsFor.${system}.nixfmt-rfc-style);

      packages = forAllSystems (
        system:
        let
          arbor = nixpkgsFor.${system}.callPackage arbor-package { };
        in
        {
          inherit arbor;

          # RDP server for the arbor session: hypr-rdp (MuNeNICK/hypr-rdp)
          # patched to run without Hyprland IPC; see nix/arbor-rdp.nix.
          arbor-rdp = nixpkgsFor.${system}.callPackage ./nix/arbor-rdp.nix { };

          # NOTE: This is for development purposes only
          #
          # It is primarily to help with quickly iterating on
          # changes made to the above expression - though it is
          # also not stripped in order to better debug arbor itself
          arbor-debug = arbor.overrideAttrs (
            newAttrs: oldAttrs: {
              pname = oldAttrs.pname + "-debug";

              cargoBuildType = "debug";
              cargoCheckType = newAttrs.cargoBuildType;

              dontStrip = true;
            }
          );

          default = arbor;
        }
      );

      overlays.default = final: _: {
        arbor = final.callPackage arbor-package { };
        arbor-rdp = final.callPackage ./nix/arbor-rdp.nix { };
      };

      nixosModules.arbor = import ./nix/module.nix self;
      nixosModules.default = self.nixosModules.arbor;
    };
}
