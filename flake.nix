{
  description = "The Mailroom: Based on the Loco (Rust) development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };

  outputs = { self, nixpkgs, rust-overlay, ... }:
    let
      system = "x86_64-linux"; # Adjust to "aarch64-linux" if on ARM
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ (import rust-overlay) ];
      };
      
      # Use a specific, stable Rust version
      rustVersion = pkgs.rust-bin.stable.latest.default;
    in
    {
      devShells.${system}.default = pkgs.mkShell {
        buildInputs = with pkgs; [
          rustVersion
          pkg-config
          openssl
          libiconv
          
          # Optional: useful for Loco DB interactions
          sea-orm-cli 
        ];

        shellHook = ''
          export PATH="$PATH:$HOME/.cargo/bin"
          export PS1="\n\[\033[1;32m\][nix-develop-shell:\w]\$ \[\033[0m\]"
          echo "🦀 Welcome to your Rust/Loco environment on NixOS!"
          echo "Rust version: $(rustc --version)"
        '';
      };
    };
}
