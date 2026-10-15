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
        rustNightly = pkgs.rust-bin.nightly."2026-10-07";
        rustToolchain = rustNightly.default.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
            "clippy"
            "llvm-tools-preview"
            "miri"
          ];
          # The visualizer (`vis/`) builds bbtree for the browser; CI tests
          # the library on wasm32-wasip1 under wasmtime.
          targets = [
            "wasm32-unknown-unknown"
            "wasm32-wasip1"
          ];
        };
      in
      {
        devShells.default = pkgs.mkShell {
          packages = [
            rustToolchain
            pkgs.pkg-config
            pkgs.cargo-fuzz
            llvmPackages.llvm
            pkgs.cargo-binutils
            # CI's wasm test runner: `CARGO_TARGET_WASM32_WASIP1_RUNNER=wasmtime
            # cargo test --target wasm32-wasip1 --lib`.
            pkgs.wasmtime
            # Exact instruction attribution (callgrind), on builds without
            # AVX-512, which valgrind can't run.
            pkgs.valgrind
            # The bench's `fetch` downloads its datasets with these, and
            # derives those it may not redistribute with
            # `bench/tools/bbtree_datasets.py`.
            pkgs.gh
            pkgs.zstd
            pkgs.python3
            # The visualizer (`vis/`): the Dioxus web toolchain. The nixpkgs
            # `dx` is built without downloads: it takes wasm-bindgen,
            # wasm-opt and tailwindcss from PATH, and needs the
            # `wasm-bindgen` crate in vis/Cargo.toml pinned to the
            # wasm-bindgen-cli it wraps.
            pkgs.dioxus-cli
            pkgs.tailwindcss_4
            pkgs.binaryen
          ];
          ASAN_SYMBOLIZER_PATH = "${llvmPackages.llvm}/bin/llvm-symbolizer";
        };

        # The perf workflow's shell on its self-hosted runner
        # (.github/workflows/perf.yml): what building and running the bench
        # with `--perf` needs, and no more, since the runner downloads it.
        devShells.bench = pkgs.mkShell {
          packages = [
            rustNightly.minimal
            pkgs.perf
          ];
        };
      }
    );
}
