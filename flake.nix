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

        pythonEnv = pkgs.python3.withPackages (ps: with ps; [
          langgraph
          langchain
          langchain-openai
          langchain-anthropic
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
          buildInputs = [ pythonEnv ];
        };
      }
    );
}