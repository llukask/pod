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
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
          libs = desktopLibs pkgs;
        in
        {
          pod-desktop = pkgs.rustPlatform.buildRustPackage {
            pname = "pod-desktop";
            version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;

            src = pkgs.lib.fileset.toSource {
              root = ./.;
              fileset = pkgs.lib.fileset.unions [
                ./.cargo
                # pod-server is a dev-dependency (the sync tests run it), and its
                # query macros read the committed cache offline.
                ./.sqlx
                # The inbox tests check the query against the shared fixtures.
                ./conformance
                ./Cargo.toml
                ./Cargo.lock
                ./crates
              ];
            };
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
            # reqwest's rustls refuses to build a client without system roots,
            # and the build sandbox has no /etc/ssl.
            preCheck = ''
              export SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt
              # jiff reads zones from the system tz database, also absent here.
              export TZDIR=${pkgs.tzdata}/share/zoneinfo
            '';

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
