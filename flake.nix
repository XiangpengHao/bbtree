{
  description = "bbtree";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      nixpkgs,
      rust-overlay,
      flake-utils,
      ...
    }:
    flake-utils.lib.eachSystem [ "x86_64-linux" ] (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };
        llvmPackages = pkgs.llvmPackages_latest;
        # Keep this exact nightly aligned with the Rust install step in CI.
        rustNightly = pkgs.rust-bin.nightly."2026-10-07".default;
        rustExtensions = [
          "rust-src"
          "rust-analyzer"
          "clippy"
          "llvm-tools-preview"
          "miri"
        ];
        rustToolchain = rustNightly.override { extensions = rustExtensions; };
      in
      {
        devShells.default = pkgs.mkShell {
          packages = [
            rustToolchain
            pkgs.pkg-config
            pkgs.cargo-fuzz
            llvmPackages.llvm
            pkgs.cargo-binutils
            # The bench's `fetch` downloads its datasets with these, and
            # derives those it may not redistribute with
            # `bench/tools/bbtree_datasets.py`.
            pkgs.gh
            pkgs.zstd
            pkgs.python3
          ];
          ASAN_SYMBOLIZER_PATH = "${llvmPackages.llvm}/bin/llvm-symbolizer";
        };

        # The visualizer (`vis/`, `nix develop .#vis`): bbtree built for
        # wasm32, and the Dioxus web toolchain. The nixpkgs `dx` is built
        # without downloads: it takes wasm-bindgen, wasm-opt and tailwindcss
        # from PATH, and needs the `wasm-bindgen` crate in vis/Cargo.toml
        # pinned to the wasm-bindgen-cli it wraps.
        devShells.vis = pkgs.mkShell {
          packages = [
            (rustNightly.override {
              extensions = rustExtensions;
              targets = [ "wasm32-unknown-unknown" ];
            })
            pkgs.dioxus-cli
            pkgs.tailwindcss_4
            pkgs.binaryen
          ];
        };
      }
    );
}
