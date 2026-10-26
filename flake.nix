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
            ];
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
