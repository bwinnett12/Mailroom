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
          pkgs.darwin.apple_sdk.frameworks.Security
          pkgs.darwin.apple_sdk.frameworks.SystemConfiguration
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
            export MAILROOM_LLM_URL="http://island.tail4b1127.ts.net:8090"
            export MAILROOM_CLASSIFY_MODEL="gpt-4"
            export MAILROOM_SUMMARISE_MODEL="gpt-4"
            export MAILROOM_CHAT_MODEL="gpt-4"
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
      # Shorthand — cfg.enable instead of config.services.mailroom.enable
    in {

      # ── Options ─────────────────────────────────────────────────────────
      # These become the public interface of the module.
      # Set them in machines/Locomotive/default.nix.
      options.services.mailroom = {

        enable = lib.mkEnableOption "Mailroom personal data routing hub";

        package = lib.mkOption {
          type        = lib.types.package;
          description = "The mailroom binary to run.";
          default     = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
          # self = this flake. Pulls the binary built by packages.default above.
          # hostPlatform.system = the system we're building FOR (aarch64-linux on Pi).
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
          default     = "http://localhost:8090";
          description = ''
            Base URL of the LocalAI instance.
            For Island over Tailscale:
              http://island.tail4b1127.ts.net:8090
          '';
        };

        classifyModel = lib.mkOption {
          type        = lib.types.str;
          default     = "gpt-4";
          description = ''
            Model used for envelope classification.
            Should be fast — runs on every unaddressed envelope.
          '';
        };

        summariseModel = lib.mkOption {
          type        = lib.types.str;
          default     = "gpt-4";
          description = "Model used for content summarisation.";
        };

        chatModel = lib.mkOption {
          type        = lib.types.str;
          default     = "gpt-4";
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
      };

      # ── Configuration (only active when enable = true) ───────────────────
      config = lib.mkIf cfg.enable {

        # ── Create directories ─────────────────────────────────────────────
        # systemd-tmpfiles runs at boot and creates these if missing.
        # 'd' = directory, 0750 = rwxr-x---, mailroom:mailroom = ownership
        systemd.tmpfiles.rules = [
          "d ${cfg.vaultPath}   0750 mailroom mailroom -"
          "d ${cfg.libraryRoot} 0750 mailroom mailroom -"
        ];

        # ── Service user ───────────────────────────────────────────────────
        # Dedicated system user — no login shell, least privilege.
        # The service runs as this user, not root.
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
          # Start automatically when the system reaches multi-user mode.

          after = [
            "network.target"
            "tailscaled.service"
            # Wait for Tailscale before starting — the LLM URL is a
            # Tailscale address and won't resolve without it.
          ];

          wants = [ "tailscaled.service" ];
          # `wants` is softer than `requires` — if tailscaled fails to
          # start, mailroom still starts. It'll just fail classification
          # until Island is reachable, which is the intended behaviour.

          environment = {
            MAILROOM_LISTEN          = cfg.listenAddr;
            MAILROOM_VAULT           = cfg.vaultPath;
            MAILROOM_LIBRARY_ROOT    = cfg.libraryRoot;
            MAILROOM_LLM_URL         = cfg.llmUrl;
            MAILROOM_CLASSIFY_MODEL  = cfg.classifyModel;
            MAILROOM_SUMMARISE_MODEL = cfg.summariseModel;
            MAILROOM_CHAT_MODEL      = cfg.chatModel;
            RUST_LOG                 = cfg.logLevel;
          };

          serviceConfig = {
            ExecStart = "${cfg.package}/bin/mailroom";
            User      = "mailroom";
            Group     = "mailroom";

            # ── Restart policy ───────────────────────────────────────
            Restart    = "on-failure";
            RestartSec = "5s";
            # Restart 5 seconds after a crash. Prevents tight crash loops
            # from hammering the CPU if something is fundamentally wrong.

            # ── Hardening ────────────────────────────────────────────
            # These restrict what the process can do even if compromised.
            NoNewPrivileges = true;
            # Process cannot gain new capabilities (e.g. via setuid).

            ProtectSystem = "strict";
            # Mounts /usr, /boot, /etc as read-only.
            # The process can't modify system files.

            ProtectHome = true;
            # /home, /root, /run/user are invisible to the process.

            ReadWritePaths = [
              cfg.vaultPath
              cfg.libraryRoot
            ];
            # Explicitly allow writes only to these directories.
            # Everything else is read-only or inaccessible.

            PrivateTmp = true;
            # The process gets its own /tmp, isolated from the system.

            PrivateDevices = true;
            # No access to physical devices (cameras, audio, etc.)
            # The Mailroom receives data over HTTP — it doesn't need
            # direct device access.
          };
        };
      };
    };
  };
}