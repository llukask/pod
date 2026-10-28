{
  description = "Pod: sync server and desktop client";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    # The Android SDK for the `android` dev shell. Its packages are the SDK
    # manager's, split per component, so the shell pulls only what the
    # Gradle build asks for.
    android-nixpkgs = {
      url = "github:tadfisher/android-nixpkgs/stable";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      android-nixpkgs,
    }:
    let
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];

      forAllSystems = nixpkgs.lib.genAttrs supportedSystems;

      # Libraries pod-desktop needs on Linux. GPUI links fontconfig, freetype
      # and xkbcommon, and opens Wayland, X11 and the Vulkan loader with
      # dlopen at runtime, so those also have to be on the library path of
      # the running binary. libmpv is for playback (design.md, "Desktop
      # client"), libdbus for souvlaki's MPRIS media controls.
      desktopLibs =
        pkgs:
        with pkgs;
        [
          fontconfig
          freetype
          libxkbcommon
          mpv-unwrapped
        ]
        ++ lib.optionals stdenv.hostPlatform.isLinux [
          wayland
          vulkan-loader
          libxcb
          dbus
        ];

      version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;

      # The workspace files every package build needs. Cargo resolves the
      # whole workspace even when it builds one package, so all crates and
      # the lockfile go in.
      workspaceFiles = [
        ./.cargo
        # The server's query macros read the committed cache offline.
        ./.sqlx
        # The protocol and inbox tests check against the shared fixtures.
        ./conformance
        ./Cargo.toml
        ./Cargo.lock
        ./crates
      ];

      workspaceSource =
        pkgs: extraFiles:
        pkgs.lib.fileset.toSource {
          root = ./.;
          fileset = pkgs.lib.fileset.unions (workspaceFiles ++ extraFiles);
        };

      # reqwest's rustls refuses to build a client without system roots, and
      # the build sandbox has no /etc/ssl. jiff reads zones from the system tz
      # database, also absent there.
      checkEnvironment = pkgs: ''
        export SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt
        export TZDIR=${pkgs.tzdata}/share/zoneinfo
      '';
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          libs = desktopLibs pkgs;
        in
        {
          pod-server = pkgs.rustPlatform.buildRustPackage {
            pname = "pod-server";
            inherit version;

            # config.rs tests that the example config parses.
            src = workspaceSource pkgs [ ./pod.example.toml ];
            cargoLock.lockFile = ./Cargo.lock;

            # `--package` limits the build, and the feature resolution, to
            # pod-server and its dependencies, so neither GPUI nor the desktop
            # libraries are compiled or linked.
            cargoBuildFlags = [
              "--package"
              "pod-server"
            ];
            # Only the test targets that need no Postgres. The build sandbox
            # has none, and `#[sqlx::test]` fails without DATABASE_URL. CI runs
            # the rest against its postgres service. Left out, all `#[sqlx::test]`
            # suites: admin, auth, auth_logs, conformance_inbox, convergence,
            # directory, housekeeping, ingest, poller, read_api, schema, seed,
            # sql_timestamp, sync_pull, sync_push, sync_subscription and
            # version. fetch_guard binds a loopback test port and needs no
            # database, so it runs.
            # TODO: a test target added to pod-server is not run here until it
            # is listed.
            cargoTestFlags = [
              "--package"
              "pod-server"
              "--lib"
              "--bins"
              "--test"
              "feed_fixtures"
              "--test"
              "feed_hash"
              "--test"
              "fetch_guard"
              "--test"
              "healthz"
            ];

            SQLX_OFFLINE = "true";
            preCheck = checkEnvironment pkgs;

            meta = {
              description = "Pod sync server";
              mainProgram = "pod-server";
              platforms = pkgs.lib.platforms.unix;
            };
          };

          pod-desktop = pkgs.rustPlatform.buildRustPackage {
            pname = "pod-desktop";
            inherit version;

            # pod-server is a dev-dependency (the sync tests run it).
            src = workspaceSource pkgs [ ];
            cargoLock.lockFile = ./Cargo.lock;

            cargoBuildFlags = [
              "--package"
              "pod-desktop"
            ];
            cargoTestFlags = [
              "--package"
              "pod-desktop"
            ];

            nativeBuildInputs = with pkgs; [
              pkg-config
              makeWrapper
            ];
            buildInputs = libs;

            # The NetworkManager and MPRIS tests start their own dbus-daemon.
            nativeCheckInputs = pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.dbus ];
            # The sync engine's live tests need Postgres, which the build
            # sandbox lacks; CI runs them against its postgres service.
            checkFlags = [
              "--skip"
              "sync::live_tests"
            ];
            SQLX_OFFLINE = "true";
            preCheck = checkEnvironment pkgs;

            postFixup = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
              wrapProgram $out/bin/pod-desktop \
                --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath libs}
            '';

            meta = {
              description = "Pod desktop client";
              mainProgram = "pod-desktop";
            };
          };

          default = self.packages.${system}.pod-desktop;
        }
      );

      nixosModules = {
        pod-server = import ./nix/module.nix { inherit self; };
        default = self.nixosModules.pod-server;
      };

      # The VM test needs a Linux host to run QEMU on.
      checks =
        nixpkgs.lib.genAttrs
          [
            "x86_64-linux"
            "aarch64-linux"
          ]
          (system: {
            pod-server = import ./nix/test.nix {
              inherit self;
              pkgs = nixpkgs.legacyPackages.${system};
            };
          });

      apps = forAllSystems (system: {
        pod-desktop = {
          type = "app";
          program = nixpkgs.lib.getExe self.packages.${system}.pod-desktop;
          meta.description = "Pod desktop client";
        };
        default = self.apps.${system}.pod-desktop;
      });

      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          libs = desktopLibs pkgs;
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              rustc
              clippy
              rustfmt
              rust-analyzer
              postgresql_16
              sqlx-cli
              cargo-insta
              shellcheck
              pkg-config
            ]
            # dbus-daemon for pod-desktop's D-Bus tests, and the headless X
            # tools behind scripts/screenshot.sh.
            ++ lib.optionals stdenv.hostPlatform.isLinux [
              dbus
              xvfb
              xauth
              xdpyinfo
              xdotool
              imagemagick
              util-linux
            ];
            buildInputs = libs;

            # rust-analyzer needs the standard library sources for go-to-definition.
            RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";

            # `cargo run -p pod-desktop` needs the libraries GPUI dlopens.
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath libs;
          };
        }
        // pkgs.lib.optionalAttrs (android-nixpkgs.sdk ? ${system}) {
          # The Android client in `android/`, entered with `nix develop
          # .#android`. Kept out of the default shell because the SDK is a
          # multi-gigabyte download the server and desktop work never needs.
          android =
            let
              androidSdk = android-nixpkgs.sdk.${system} (
                sdkPkgs: with sdkPkgs; [
                  # 23+ ships a native `android` CLI the sdk wrapper can't
                  # exec (tadfisher/android-nixpkgs#134); 19 is plain Java.
                  cmdline-tools-19-0
                  platform-tools
                  platforms-android-36
                  build-tools-36-0-0
                ]
              );
            in
            pkgs.mkShell {
              # AGP 9 runs on JDK 21.
              packages = [
                androidSdk
                pkgs.jdk21
              ];
              JAVA_HOME = pkgs.jdk21.home;

              # android-nixpkgs exports ANDROID_HOME itself. AGP otherwise
              # downloads a generic, dynamically linked aapt2 from Maven
              # whose daemon can't start on NixOS. Gradle maps the
              # `org.gradle.project.` system property prefix onto project
              # properties, so GRADLE_OPTS can carry a store path.
              shellHook = ''
                export ANDROID_AVD_HOME="$HOME/.android/avd"
                export GRADLE_OPTS="''${GRADLE_OPTS:+$GRADLE_OPTS }-Dorg.gradle.project.android.aapt2FromMavenOverride=$ANDROID_HOME/build-tools/36.0.0/aapt2"
              '';
            };
        }
      );
    };
}
