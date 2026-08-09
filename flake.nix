{
  description = "Feynman Chiron — Rust agent backend + Rust textbook ingest";

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

        # ── Rust workspace (chiron-rs agent + chiron-ingest, statically linked) ──
        #
        # On Linux: musl via pkgsStatic → fully static binaries. hf-hub's
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
        #
        # buildRustPackage builds every workspace member by default, so this
        # one derivation's $out/bin has both chiron-rs and chiron-ingest.
        chiron = staticPkgs.rustPlatform.buildRustPackage ({
          pname   = "feynman-chiron";
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

      in {
        # nix build  →  result/bin/{chiron-rs,chiron-ingest}  (static musl, no glibc dep)
        packages.default       = chiron;
        packages.chiron-rs     = chiron;
        packages.chiron-ingest = chiron;

        # nix develop  (dynamic, for cargo build --release during development)
        devShells.default = pkgs.mkShell {
          buildInputs = [
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
            echo "  Rust: $(rustc --version)"
            echo ""
            echo "Build (dynamic):  cargo build --release   (in chiron-rs/, builds both binaries)"
            echo "Build (static):   nix build"
          '';
        };
      }
    );
}
