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
          # The visualizer (`bbtree/vis/`) builds bbtree for the browser; CI
          # tests the library on wasm32-wasip1 under wasmtime.
          targets = [
            "wasm32-unknown-unknown"
            "wasm32-wasip1"
          ];
        };
        # Verus proves bbtree/src/proof/ (design/08-fuzzing-verification.md).
        # Keep the version in sync with the `vstd` pin in bbtree/Cargo.toml,
        # and verusRustToolchain with the release's rust-toolchain.toml.
        verusVersion = "0.2026.08.30.b432e82";
        verusRustToolchain = pkgs.rust-bin.stable."1.97.1".default.override {
          extensions = [
            "rustc-dev"
            "llvm-tools"
            "rust-src"
          ];
        };
        verus = pkgs.stdenvNoCC.mkDerivation {
          pname = "verus";
          version = verusVersion;

          src = pkgs.fetchzip {
            url = "https://github.com/verus-lang/verus/releases/download/release%2F${verusVersion}/verus-${verusVersion}-x86-linux.zip";
            hash = "sha256-VMM0e9/Gb6fBFIqGoZaB+XLdwph2s5KuIdX6gmSY/ss=";
            stripRoot = false;
          };

          strictDeps = true;

          nativeBuildInputs = [
            pkgs.makeWrapper
            pkgs.autoPatchelfHook
          ];
          buildInputs = [
            pkgs.zlib
            pkgs.stdenv.cc.cc.lib
          ];
          autoPatchelfIgnoreMissingDeps = [
            "librustc_driver*"
            "libLLVM*"
            "libstd-*"
          ];

          installPhase = ''
            runHook preInstall

            mkdir -p "$out/bin"
            cp -r verus-x86-linux/* "$out"/

            mv "$out/verus" "$out/verus-bin"
            mv "$out/cargo-verus" "$out/cargo-verus-bin"

            makeWrapper "$out/rust_verify" "$out/verus" \
              --set VERUS_ROOT "$out" \
              --set VERUS_Z3_PATH "$out/z3" \
              --prefix LD_LIBRARY_PATH : "${verusRustToolchain}/lib"

            makeWrapper "$out/cargo-verus-bin" "$out/cargo-verus" \
              --set VERUS_ROOT "$out" \
              --set VERUS_Z3_PATH "$out/z3" \
              --prefix LD_LIBRARY_PATH : "${verusRustToolchain}/lib"

            chmod +x "$out/rust_verify" "$out/z3" "$out/cargo-verus-bin" "$out/verus-bin" || true

            ln -s "$out/verus" "$out/bin/verus"
            ln -s "$out/cargo-verus" "$out/bin/cargo-verus"
            ln -s "$out/rust_verify" "$out/bin/rust_verify"
            ln -s "$out/z3" "$out/bin/z3"

            runHook postInstall
          '';
        };
        verusfmt = pkgs.rustPlatform.buildRustPackage (finalAttrs: {
          pname = "verusfmt";
          version = "0.7.2";

          src = pkgs.fetchFromGitHub {
            owner = "verus-lang";
            repo = "verusfmt";
            tag = "v${finalAttrs.version}";
            hash = "sha256-TE1Qyk5y8G/Kid6/BmUIMZ8Fr+y8GkLkRrzXGFHVe7I=";
          };

          cargoHash = "sha256-QY8Sju3AzfGiSp6V2TsuUlT1EmW3rOdhp3EeU1XM3Bg=";

          # Its tests check that rustfmt leaves `verus!` blocks alone.
          nativeCheckInputs = [
            pkgs.cargo
            pkgs.rustfmt
          ];
        });
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
            # Backtraces of every thread of a hung concurrent test
            # (`BBTreeMap`): `gdb -p <pid> -batch -ex "thread apply all bt"`.
            pkgs.gdb
            # The bench's `fetch` downloads its datasets with these, and
            # derives those it may not redistribute with
            # `bbtree/bench/tools/bbtree_datasets.py`.
            pkgs.gh
            pkgs.zstd
            pkgs.python3
            # The visualizer (`bbtree/vis/`): the Dioxus web toolchain. The
            # nixpkgs `dx` is built without downloads: it takes wasm-bindgen,
            # wasm-opt and tailwindcss from PATH, and needs the
            # `wasm-bindgen` crate in bbtree/vis/Cargo.toml pinned to the
            # wasm-bindgen-cli it wraps.
            pkgs.dioxus-cli
            pkgs.tailwindcss_4
            pkgs.binaryen
            verus
            verusfmt
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

        # The Verus workflow's shell (.github/workflows/verus.yml): cargo, and
        # Verus with the toolchain of its own it brings.
        devShells.verus = pkgs.mkShell {
          packages = [
            rustNightly.minimal
            verus
          ];
        };

        # The fuzzing workflow's shell on the same runner
        # (.github/workflows/fuzz.yml): the nightly that builds the targets
        # with AddressSanitizer, cargo-fuzz, the symbolizer for the
        # sanitizer's stack traces, and gh, which fetches the corpora the
        # last run saved.
        devShells.fuzz = pkgs.mkShell {
          packages = [
            rustNightly.minimal
            pkgs.cargo-fuzz
            pkgs.gh
          ];
          ASAN_SYMBOLIZER_PATH = "${llvmPackages.llvm}/bin/llvm-symbolizer";
        };
      }
    );
}
