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
        # musl-based package set — all C libs compiled as static musl. On
        # Linux this produces a fully static binary; on Darwin, pkgsStatic
        # links everything reachable statically except Apple system
        # frameworks (there is no static libc equivalent there).
        staticPkgs = pkgs.pkgsStatic;

        # ── Rust package (statically linked musl binary) ─────────────────────
        #
        # On Linux: musl via pkgsStatic → fully static binary. hf-hub's
        # HTTP client links against OpenSSL; pkgsStatic provides the static
        # libssl/libcrypto. On macOS: Security.framework is a system
        # framework linked automatically, no extra inputs needed (same
        # pattern as kamysh/mimir and kamysh/muninn's flakes).
        #
        # tokenizers pulls in `esaxx-rs` (C++, always — independent of any
        # feature flag), so the binary links libstdc++. On x86_64 musl,
        # libstdc++.a's eh_personality.o has R_X86_64_32S relocations that
        # can't appear in a PIE, and pkgsStatic defaults to -static-pie — so
        # disable PIE for that one target. darwin and aarch64-linux link
        # fine without this (verified in muninn's flake.nix, same esaxx-rs
        # dependency).
        chiron-rs = staticPkgs.rustPlatform.buildRustPackage ({
          pname   = "chiron-rs";
          version = "0.1.0";
          src     = ./chiron-rs;

          cargoLock.lockFile = ./chiron-rs/Cargo.lock;

          buildInputs = pkgs.lib.optionals (pkgs.lib.hasSuffix "linux" system) [
            staticPkgs.openssl
          ];
          nativeBuildInputs = pkgs.lib.optionals (pkgs.lib.hasSuffix "linux" system) [
            pkgs.pkg-config
          ];
        } // pkgs.lib.optionalAttrs (pkgs.lib.hasSuffix "linux" system) {
          PKG_CONFIG_PATH       = "${staticPkgs.openssl.dev}/lib/pkgconfig";
          PKG_CONFIG_ALL_STATIC = "1";
          OPENSSL_STATIC        = "1";
          OPENSSL_LIB_DIR       = "${staticPkgs.openssl.out}/lib";
          OPENSSL_INCLUDE_DIR   = "${staticPkgs.openssl.dev}/include";
        } // pkgs.lib.optionalAttrs (system == "x86_64-linux") {
          RUSTFLAGS = "-C relocation-model=static";
        });

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
            pkgs.openssl
          ];

          PKG_CONFIG_PATH = "${pkgs.openssl.dev}/lib/pkgconfig";
          LD_LIBRARY_PATH = "${pkgs.openssl}/lib";

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
