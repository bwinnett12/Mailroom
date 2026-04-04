
{
  description = "Julia Data Science Environment for NixOS";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, utils }:
    # This function returns a set keyed by system (e.g., packages.x86_64-linux)
    (utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          config.allowUnfree = true;
        };

        # Essential libraries for Julia's Artifacts (plotting, SSL, compression)
        julia-libs = with pkgs; [
          stdenv.cc.cc.lib
          zlib
          glib
          libX11
          libXext
          libXrender
          libICE
          libSM
          libGL
          fontconfig
          freetype
        ];
      in
      {

        packages.default = pkgs.stdenv.mkDerivation {
          name = "mailroom-app";
          src = ./.;
          buildInputs = [ pkgs.julia-bin ];
          installPhase = ''
            mkdir -p $out/share/mailroom
            cp -r . $out/share/mailroom
          '';
        };

        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [
            julia-bin # The official binary distribution #TODO - Maybe this should be set to a version then adjusted incrementally
            git
          ];

          ### Replace statement with statement below and it will reset
          # export LD_LIBRARY_PATH=${pkgs.lib.makeLibraryPath julia-libs}:$LD_LIBRARY_PATH


          # This is where the dynamic linker finds the libraries that Julia's downloaded artifacts needs.
          shellHook = ''
            export LD_LIBRARY_PATH=${pkgs.lib.makeLibraryPath [ pkgs.stdenv.cc.cc.lib pkgs.zlib ]}

            echo "--- Julia Genie Environment Loaded ---"
            echo "Run 'julia' and then 'using Pkg; Pkg.instantiate()' to begin."
            echo "Mailroom Dev Environment Ready"
          '';
        };
      }
    )) // {
      nixosModules.mailroom = import ./nixos-module.nix;
    };
}


