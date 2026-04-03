{
  description = "Julia Data Science Environment for NixOS";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, utils }:
    utils.lib.eachDefaultSystem (system:
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
          xorg.libX11
          xorg.libXext
          xorg.libXrender
          xorg.libICE
          xorg.libSM
          libGL
          fontconfig
          freetype
        ];
      in
      {
        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [
            julia-bin # The official binary distribution
            git
          ];

          # This is the "magic" for NixOS: it tells the dynamic linker where
          # to find the libraries that Julia's downloaded artifacts need.
          shellHook = ''
            export LD_LIBRARY_PATH=${pkgs.lib.makeLibraryPath julia-libs}:$LD_LIBRARY_PATH
            echo "--- Julia Data Science Environment Loaded ---"
            echo "Run 'julia' and then 'using Pkg; Pkg.instantiate()' to begin."
          '';
        };
      });
}