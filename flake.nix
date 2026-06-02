{
  description = "Feynman Chiron — Rust agent backend + Python textbook ingest";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};

        # Native C deps shared between package build and dev shell
        nativeDeps = [ pkgs.pkg-config ];
        buildDeps  = [ pkgs.oniguruma pkgs.openssl ];  # tokenizers "onig" feature; hf-hub native-tls

        # ── Rust package ─────────────────────────────────────────────────────
        chiron-rs = pkgs.rustPlatform.buildRustPackage {
          pname   = "chiron-rs";
          version = "0.1.0";
          src     = ./chiron-rs;

          cargoLock.lockFile = ./chiron-rs/Cargo.lock;

          nativeBuildInputs = nativeDeps;
          buildInputs       = buildDeps;

          PKG_CONFIG_PATH = "${pkgs.oniguruma}/lib/pkgconfig:${pkgs.openssl.dev}/lib/pkgconfig";
        };

        # ── Python env for textbook ingestion (chiron_storage.py) ────────────
        pythonEnv = pkgs.python3.withPackages (ps: with ps; [
          langchain-community
          psycopg2
          pypdf
          numpy
          sentence-transformers
          pytest
          pytest-mock
        ]);

      in {
        # nix build  →  result/bin/chiron-rs
        packages.default = chiron-rs;
        packages.chiron-rs = chiron-rs;

        # nix develop
        devShells.default = pkgs.mkShell {
          buildInputs = [
            pythonEnv
            pkgs.cargo
            pkgs.rustc
            pkgs.rust-analyzer
            pkgs.clippy
          ] ++ nativeDeps ++ buildDeps;

          PKG_CONFIG_PATH  = "${pkgs.oniguruma}/lib/pkgconfig:${pkgs.openssl.dev}/lib/pkgconfig";
          LD_LIBRARY_PATH  = "${pkgs.oniguruma}/lib:${pkgs.openssl}/lib";

          shellHook = ''
            echo "Feynman Chiron dev shell"
            echo "  Python (ingest): $(python3 --version)"
            echo "  Rust (agent):    $(rustc --version)"
            echo ""
            echo "Build:  cargo build --release   (in chiron-rs/)"
            echo "Or:     nix build"
          '';
        };
      }
    );
}
