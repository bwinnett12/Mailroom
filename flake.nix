# Mailroom/flake.nix
# 82_Mailroom — Axum routing server
# Repository: github:bwinnett12/Mailroom
#
# Exports:
#   packages.default        ← the mailroom binary
#   nixosModules.default    ← NixOS service module
#   devShells.default       ← development environment

{
  description = "Mailroom — personal data routing hub (82_Mailroom)";

  inputs = {
    nixpkgs.url      = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url  = "github:numtide/flake-utils";
    # Both rust-overlay and flake-utils are overridden by the parent
    # flake via `follows` — these urls are fallbacks for when this
    # flake is used standalone (e.g. nix develop on your Mac).
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils, ... }:
  let
    # ── System-specific outputs ─────────────────────────────────────────────
    # eachDefaultSystem generates outputs for every system in the list.
    # Your Pi is aarch64-linux, your Mac is aarch64-darwin or x86_64-darwin,
    # Island is x86_64-linux. All three work with one flake.
    perSystemOutputs = flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };

        # Stable Rust with IDE support.
        # No wasm targets — Mailroom is a server binary only.
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" ];
        };

        # Dependencies needed to build reqwest with native TLS.
        # Split into build-time (nativeBuildInputs) and
        # link-time (buildInputs) — Nix is strict about this distinction.
        nativeBuildInputs = with pkgs; [
          pkg-config
          # pkg-config finds openssl at build time
        ];

        buildInputs = with pkgs; [
          openssl
          # reqwest uses openssl for HTTPS to LocalAI
        ] ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
          pkgs.libiconv
          pkgs.libiconv
          pkgs.apple-sdk_11
          # macOS requires these frameworks for TLS.
          # Harmless on Linux — optionals means they're skipped there.
        ];

      in {
        # ── Dev shell ───────────────────────────────────────────────────────
        # Enter with: nix develop
        # Gives you rustc, cargo, rust-analyzer, and all build deps.
        # All MAILROOM_* env vars are pre-set for local development.
        devShells.default = pkgs.mkShell {
          inherit nativeBuildInputs;
          buildInputs = [ rustToolchain ] ++ buildInputs;

          shellHook = ''
            export PATH="$PATH:$HOME/.cargo/bin"
            export PS1="\n\[\033[1;32m\][mailroom:\w]\$ \[\033[0m\]"

            # ── Dev environment variables ───────────────────────────────
            # Override any of these in your shell before running cargo run.
            export MAILROOM_LISTEN="0.0.0.0:3000"
            export MAILROOM_VAULT="/tmp/test-vault"
            export MAILROOM_LIBRARY_ROOT="/tmp/test-library"
            export MAILROOM_LLM_URL="http://ai.platatoo.com"
            export MAILROOM_CLASSIFY_MODEL="qwen_qwen3.5-0.8b"
            export MAILROOM_SUMMARISE_MODEL="qwen_qwen3.5-0.8b"
            export MAILROOM_CHAT_MODEL="qwen_qwen3.5-0.8b"
            export RUST_LOG="mailroom=debug,tower_http=info"
            # debug = verbose, shows every routing decision
            # Change to "mailroom=info" for quieter output

            echo ""
            echo "📬 Mailroom dev shell"
            echo "   Rust:    $(rustc --version)"
            echo "   LLM:     $MAILROOM_LLM_URL"
            echo "   Vault:   $MAILROOM_VAULT"
            echo "   Library: $MAILROOM_LIBRARY_ROOT"
            echo ""
            echo "   cargo run        → start the server"
            echo "   cargo build      → compile only"
            echo "   cargo test       → run tests"
            echo "   cargo clippy     → lint"
            echo ""
          '';
        };

        # ── Binary package ──────────────────────────────────────────────────
        # Build with: nix build
        # Result at: ./result/bin/mailroom
        #
        # This is what the NixOS module installs on Locomotive.
        # Nix builds it in a sandbox — no network access, reproducible.
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname   = "mailroom";
          version = "0.1.0";

          src = ./.;
          # Build from the current directory.
          # In the NixOS module, Nix fetches this from GitHub instead.

          cargoLock.lockFile = ./Cargo.lock;
          # Nix uses Cargo.lock to fetch exact dependency versions.
          # Always commit Cargo.lock — this won't work without it.

          inherit nativeBuildInputs buildInputs;

          # Tell openssl-sys to use the system openssl rather than
          # trying to compile its own. Required in the Nix sandbox.
          OPENSSL_NO_VENDOR = 1;
        };
      }
    );

  in
  # ── Merge per-system outputs with system-agnostic outputs ──────────────────
  perSystemOutputs // {

    # ── NixOS module ─────────────────────────────────────────────────────────
    # nixosModules lives outside eachDefaultSystem because NixOS modules
    # aren't tied to the build system — they describe configuration,
    # not compiled artifacts.
    #
    # Import in machines/Locomotive/default.nix:
    #   imports = [ inputs.mailroom.nixosModules.default ];
        nixosModules.default = { config, lib, pkgs, ... }:
    let
      cfg = config.services.mailroom;
    in {

      # ── Options ─────────────────────────────────────────────────────────
      options.services.mailroom = {

        enable = lib.mkEnableOption "Mailroom personal data routing hub";

        package = lib.mkOption {
          type        = lib.types.package;
          description = "The mailroom binary to run.";
          default     = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
        };

        listenAddr = lib.mkOption {
          type        = lib.types.str;
          default     = "0.0.0.0:3000";
          description = ''
            Address and port the Mailroom binds to.
            Tailscale-only deployment: 0.0.0.0 is safe since the Pi
            isn't directly internet-exposed.
          '';
        };

        vaultPath = lib.mkOption {
          type        = lib.types.str;
          default     = "/var/lib/mailroom/vault";
          description = ''
            Path to the local clone of your Obsidian vault.
            The Mailroom reads .mailroom manifests from here to build
            the registry at startup.
          '';
        };

        libraryRoot = lib.mkOption {
          type        = lib.types.str;
          default     = "/var/lib/mailroom/library";
          description = ''
            Root directory where envelope content is written.
            On Pi: local storage.
            Eventually: /storage/Library (9P mount from Island).
          '';
        };

        llmUrl = lib.mkOption {
          type        = lib.types.str;
          default     = "http://ai.platatoo.com";
          description = ''
            Base URL of the LocalAI instance.
            For Island over Tailscale:
              http://ai.platatoo.com
          '';
        };

        classifyModel = lib.mkOption {
          type        = lib.types.str;
          default     = "qwen_qwen3.5-0.8b";
          description = ''
            Model used for envelope classification.
            Should be fast — runs on every unaddressed envelope.
          '';
        };

        summariseModel = lib.mkOption {
          type        = lib.types.str;
          default     = "qwen_qwen3.5-0.8b";
          description = "Model used for content summarisation.";
        };

        chatModel = lib.mkOption {
          type        = lib.types.str;
          default     = "qwen_qwen3.5-0.8b";
          description = ''
            Model used for chat completions via /v1/chat/completions.
            JD routing may override this per-request.
          '';
        };

        logLevel = lib.mkOption {
          type        = lib.types.str;
          default     = "mailroom=info,tower_http=info";
          description = ''
            RUST_LOG filter string.
            Use "mailroom=debug" for verbose routing logs.
            Use "mailroom=info" for production.
          '';
        };

        # ── NEW OPTION ───────────────────────────────────────────────────
        configFile = lib.mkOption {
          type        = lib.types.nullOr lib.types.path;
          default     = null;
          description = ''
            Path to Mailroom.toml configuration file.
            If set, Mailroom will load this file instead of relying on 
            individual MAILROOM_* environment variables.
            
            If null, Mailroom falls back to env-var mode and reads:
              - MAILROOM_LISTEN
              - MAILROOM_VAULT
              - MAILROOM_LIBRARY_ROOT
              - MAILROOM_LLM_URL
              - MAILROOM_CLASSIFY_MODEL
              - MAILROOM_SUMMARISE_MODEL
              - MAILROOM_CHAT_MODEL
          '';
        };
      };

      # ── Configuration (only active when enable = true) ───────────────────
      config = lib.mkIf cfg.enable {

        # ── Create directories ─────────────────────────────────────────────
        systemd.tmpfiles.rules = [
          "d ${cfg.vaultPath}   0750 mailroom mailroom -"
          "d ${cfg.libraryRoot} 0750 mailroom mailroom -"
        ];

        # ── Service user ───────────────────────────────────────────────────
        users.users.mailroom = {
          isSystemUser = true;
          group        = "mailroom";
          description  = "Mailroom service user (82_Mailroom)";
        };
        users.groups.mailroom = {};

        # ── systemd service ────────────────────────────────────────────────
        systemd.services.mailroom = {
          description = "82_Mailroom — personal data routing hub";
          wantedBy    = [ "multi-user.target" ];

          after = [
            "network.target"
            "tailscaled.service"
          ];

          wants = [ "tailscaled.service" ];

          # ── Environment variables ──────────────────────────────────────
          # Merge base env vars with optional MAILROOM_CONFIG if configFile is set
          environment = 
            {
              MAILROOM_LISTEN          = cfg.listenAddr;
              MAILROOM_VAULT           = cfg.vaultPath;
              MAILROOM_LIBRARY_ROOT    = cfg.libraryRoot;
              MAILROOM_LLM_URL         = cfg.llmUrl;
              MAILROOM_CLASSIFY_MODEL  = cfg.classifyModel;
              MAILROOM_SUMMARISE_MODEL = cfg.summariseModel;
              MAILROOM_CHAT_MODEL      = cfg.chatModel;
              RUST_LOG                 = cfg.logLevel;
            } // lib.optionalAttrs (cfg.configFile != null) {
              # ── NEW: Conditionally set MAILROOM_CONFIG ───────────────
              # Only set if configFile option is not null
              MAILROOM_CONFIG = cfg.configFile;
            };

          serviceConfig = {
            ExecStart = "${cfg.package}/bin/mailroom";
            User      = "mailroom";
            Group     = "mailroom";

            # ── Restart policy ───────────────────────────────────────
            Restart    = "on-failure";
            RestartSec = "5s";

            # ── Hardening ────────────────────────────────────────────
            NoNewPrivileges = true;
            ProtectSystem = "strict";
            ProtectHome = true;
            ReadWritePaths = [
              cfg.vaultPath
              cfg.libraryRoot
            ];
            PrivateTmp = true;
            PrivateDevices = true;
          };
        };
      };
    };
  };
}