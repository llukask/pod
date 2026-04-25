{
  description = "Build and run the Pod web frontend (Leptos / wasm32)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};

        # ====================================================================
        # Toolchain
        # ====================================================================
        # rustc + cargo
        #   nixpkgs ships rustc with `wasm32-unknown-unknown` already in
        #   `lib/rustlib`, so no rustup / `rustup target add` is needed.
        # trunk
        #   orchestrates the wasm build: invokes cargo, runs wasm-bindgen,
        #   optionally wasm-opt, and bundles the static assets referenced
        #   by `crates/pod-web/index.html` into `frontend/dist/`.
        # wasm-bindgen-cli
        #   Trunk auto-downloads this on first run if it isn't on PATH.
        #   Pinning it through Nix avoids the network round-trip and keeps
        #   the build reproducible. If the nixpkgs version drifts from the
        #   `wasm-bindgen` crate in `crates/pod-web/Cargo.lock`, Trunk will
        #   refuse to use it and fall back to downloading — bump nixpkgs
        #   (or pin a matching `wasm-bindgen-cli_0_2_x` attribute) when
        #   that happens.
        # binaryen
        #   provides `wasm-opt`. Trunk runs it automatically on
        #   `--release` builds to shrink the bundle.
        # lld
        #   the linker rustc shells out to for the wasm target. Without
        #   it, cargo fails with `linker \`lld\` not found`.
        toolchain = with pkgs; [
          rustc
          cargo
          trunk
          wasm-bindgen-cli
          binaryen
          lld
        ];
      in
      {
        # `nix develop` drops you into a shell with the full toolchain on
        # PATH so the existing `trunk build` / `trunk serve` workflow
        # documented in README.md works without any host setup.
        devShells.default = pkgs.mkShell {
          packages = toolchain ++ [ pkgs.cargo-watch ];

          shellHook = ''
            echo "pod-web devshell — Leptos / wasm32 frontend"
            echo
            echo "Common commands:"
            echo "  cd crates/pod-web && trunk build --release   # bundle into frontend/dist/"
            echo "  cd crates/pod-web && trunk serve             # hot-reload dev server on :8080"
            echo
            echo "Or via this flake:"
            echo "  nix run .#build       # one-shot release build"
            echo "  nix run .#serve       # start the dev server"
          '';
        };

        # ====================================================================
        # Apps — convenience shortcuts that bake the toolchain into PATH
        # ====================================================================
        # `nix run .#build`  → produce frontend/dist
        # `nix run .#serve`  → start the dev server (proxies /api/v1 → :3000
        #                      per crates/pod-web/Trunk.toml)
        # `nix run`           → same as `nix run .#serve`
        #
        # The scripts cd into `crates/pod-web` so they pick up its
        # `Trunk.toml`, then exec trunk with any extra arguments forwarded
        # through. They must be invoked from the repo root.
        apps = {
          build = {
            type = "app";
            program = toString (pkgs.writeShellApplication {
              name = "pod-web-build";
              runtimeInputs = toolchain;
              text = ''
                cd crates/pod-web
                exec trunk build --release "$@"
              '';
            });
          };

          serve = {
            type = "app";
            program = toString (pkgs.writeShellApplication {
              name = "pod-web-serve";
              runtimeInputs = toolchain;
              text = ''
                cd crates/pod-web
                exec trunk serve "$@"
              '';
            });
          };

          default = self.apps.${system}.serve;
        };
      });
}
