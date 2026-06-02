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
        # musl-based package set — all C libs compiled as static musl
        staticPkgs = pkgs.pkgsStatic;

        # ── Rust package (statically linked musl binary) ─────────────────────
        chiron-rs = staticPkgs.rustPlatform.buildRustPackage {
          pname   = "chiron-rs";
          version = "0.1.0";
          src     = ./chiron-rs;

          cargoLock.lockFile = ./chiron-rs/Cargo.lock;

          # pkg-config runs on the build host, not the musl target
          nativeBuildInputs = [ pkgs.pkg-config ];

          # C deps as static musl libraries
          buildInputs = [
            staticPkgs.oniguruma   # tokenizers "onig" feature
            staticPkgs.openssl     # hf-hub → native-tls → openssl-sys
          ];

          PKG_CONFIG_PATH    = "${staticPkgs.oniguruma}/lib/pkgconfig:${staticPkgs.openssl.dev}/lib/pkgconfig";
          PKG_CONFIG_ALL_STATIC = "1";
          OPENSSL_STATIC      = "1";
          OPENSSL_LIB_DIR     = "${staticPkgs.openssl.out}/lib";
          OPENSSL_INCLUDE_DIR = "${staticPkgs.openssl.dev}/include";
          # esaxx-rs (tokenizers dep, C++) isn't compiled with -fPIE; use
          # plain -static instead of -static-pie to allow non-PIC relocations.
          RUSTFLAGS           = "-C relocation-model=static";
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
        # nix build  →  result/bin/chiron-rs  (static musl binary, no glibc dep)
        packages.default = chiron-rs;
        packages.chiron-rs = chiron-rs;

        # nix develop  (dynamic, for cargo build --release during development)
        devShells.default = pkgs.mkShell {
          buildInputs = [
            pythonEnv
            pkgs.cargo
            pkgs.rustc
            pkgs.rust-analyzer
            pkgs.clippy
            pkgs.pkg-config
            pkgs.oniguruma
            pkgs.openssl
          ];

          PKG_CONFIG_PATH = "${pkgs.oniguruma}/lib/pkgconfig:${pkgs.openssl.dev}/lib/pkgconfig";
          LD_LIBRARY_PATH = "${pkgs.oniguruma}/lib:${pkgs.openssl}/lib";

          shellHook = ''
            echo "Feynman Chiron dev shell"
            echo "  Python (ingest): $(python3 --version)"
            echo "  Rust (agent):    $(rustc --version)"
            echo ""
            echo "Build (dynamic):  cargo build --release   (in chiron-rs/)"
            echo "Build (static):   nix build"
          '';
        };
      }
    );
}
