{
  description = "Pod: sync server and desktop client";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
  };

  outputs =
    { self, nixpkgs }:
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
      # client").
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
            # version. directory and poller also bind fixed ports, but only
            # beside a database. fetch_guard binds 127.0.0.1:3007 and needs no
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

            # The NetworkManager monitor test starts its own dbus-daemon. It is
            # `#[ignore]`d because the dev sandbox denies the daemon's logind
            # lookup; the Nix build sandbox has no logind, so it runs here.
            nativeCheckInputs = pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.dbus ];
            # The sync engine's live tests need Postgres, which the build
            # sandbox lacks; CI runs them against its postgres service.
            checkFlags = [
              "--include-ignored"
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
            # dbus-daemon for `cargo test -p pod-desktop -- --include-ignored`.
            ++ lib.optionals stdenv.hostPlatform.isLinux [ dbus ];
            buildInputs = libs;

            # rust-analyzer needs the standard library sources for go-to-definition.
            RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";

            # `cargo run -p pod-desktop` needs the libraries GPUI dlopens.
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath libs;
          };
        }
      );
    };
}
