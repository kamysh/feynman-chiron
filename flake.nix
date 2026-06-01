{
  description = "Feynman Chiron development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = nixpkgs.legacyPackages.${system};

        # Python env: kept for textbook ingestion (chiron_storage.py)
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
        devShells.default = pkgs.mkShell {
          buildInputs = [
            pythonEnv
            # Rust toolchain for chiron-rs
            pkgs.cargo
            pkgs.rustc
            pkgs.rust-analyzer
            pkgs.clippy
            # Native deps needed by Rust crates
            pkgs.pkg-config
            pkgs.openssl
            pkgs.postgresql
            pkgs.oniguruma   # for tokenizers "onig" feature
          ];

          # Linker picks up openssl, libpq, libonig
          PKG_CONFIG_PATH = "${pkgs.openssl.dev}/lib/pkgconfig:${pkgs.postgresql.lib}/lib/pkgconfig:${pkgs.oniguruma}/lib/pkgconfig";
          LD_LIBRARY_PATH = "${pkgs.openssl.out}/lib:${pkgs.postgresql.lib}/lib:${pkgs.oniguruma}/lib";

          shellHook = ''
            echo "Feynman Chiron dev shell"
            echo "  Python (ingest): $(python3 --version)"
            echo "  Rust (agent):    $(rustc --version)"
            echo ""
            echo "Build chiron-rs:"
            echo "  cd chiron-rs && cargo build --release"
          '';
        };
      }
    );
}
