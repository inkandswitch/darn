{
  description = "darn - Distributed Automerge Resource Navigator";

  inputs = {
    nixpkgs.url = "nixpkgs/nixos-25.11";

    command-utils.url = "git+https://codeberg.org/expede/nix-command-utils";
    flake-utils.url = "github:numtide/flake-utils";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = {
    self,
    flake-utils,
    nixpkgs,
    rust-overlay,
    command-utils,
  } @ inputs:
    flake-utils.lib.eachDefaultSystem (
      system: let
        overlays = [
          (import rust-overlay)
        ];

        pkgs = import nixpkgs {
          inherit system overlays;
        };

        rustVersion = "1.90.0";

        rust-toolchain = pkgs.rust-bin.stable.${rustVersion}.default.override {
          extensions = [
            "cargo"
            "clippy"
            "llvm-tools-preview"
            "rust-src"
            "rust-std"
          ];

          targets = [
            "aarch64-apple-darwin"
            "x86_64-apple-darwin"
            "x86_64-unknown-linux-musl"
            "aarch64-unknown-linux-musl"
          ];
        };

        nightly-rustfmt = pkgs.rust-bin.nightly."2026-02-23".default.override {
          extensions = ["rustfmt"];
        };

        format-pkgs = with pkgs; [
          alejandra
          nixpkgs-fmt
          taplo
        ];

        cargo-installs = with pkgs; [
          cargo-deny
          cargo-expand
          cargo-nextest
          cargo-outdated
          cargo-sort
          cargo-udeps
          cargo-watch
        ];

        rust = command-utils.rust.${system};
        cmd = command-utils.cmd.${system};

        command_menu = command-utils.commands.${system} [
          (rust.build {cargo = pkgs.cargo;})
          (rust.test {
            cargo = pkgs.cargo;
            cargo-watch = pkgs.cargo-watch;
          })
          (rust.lint {cargo = pkgs.cargo;})
          (rust.fmt {cargo = pkgs.cargo;})
          (rust.doc {cargo = pkgs.cargo;})
          (rust.watch {cargo-watch = pkgs.cargo-watch;})
        ];
      in rec {
        packages = {
          darn = pkgs.rustPlatform.buildRustPackage {
            pname = "darn";
            version = "0.3.0";
            meta = {
              description = "Distributed Automerge Resource Navigator";
              longDescription = ''
                A filesystem CLI for managing CRDT-backed files using
                Subduction (P2P sync) and Automerge (CRDT documents).
                Enables local-first, collaborative file management with
                automatic conflict resolution and peer-to-peer synchronization.
              '';
              homepage = "https://github.com/inkandswitch/darn";
              license = [
                pkgs.lib.licenses.mit
                pkgs.lib.licenses.asl20
              ];
              maintainers = [pkgs.lib.maintainers.expede];
              platforms = pkgs.lib.platforms.unix;
              mainProgram = "darn";
            };

            src = ./.;

            cargoLock = {
              lockFile = ./Cargo.lock;
              # outputHashes = {};
            };

            buildInputs = pkgs.lib.optionals pkgs.stdenv.isLinux [pkgs.openssl];
            nativeBuildInputs = [pkgs.pkg-config];

            cargoBuildFlags = ["--bin" "darn"];

            doCheck = !pkgs.stdenv.buildPlatform.canExecute pkgs.stdenv.hostPlatform;

            checkPhase = ''
              cargo test --release --locked
            '';
          };

          default = packages.darn;
        };

        devShells.default = pkgs.mkShell {
          name = "darn_shell";

          nativeBuildInputs =
            [
              command_menu
              rust-toolchain
              nightly-rustfmt

              pkgs.rust-analyzer
              pkgs.websocat
            ]
            ++ format-pkgs
            ++ cargo-installs
            ++ pkgs.lib.optionals pkgs.stdenv.isLinux [
              pkgs.clang
              pkgs.llvmPackages.libclang
              pkgs.openssl.dev
              pkgs.pkg-config
            ];

          shellHook =
            ''
              unset SOURCE_DATE_EPOCH
              export WORKSPACE_ROOT="$(pwd)"
              export RUSTFMT="${nightly-rustfmt}/bin/rustfmt"
              export DYLD_LIBRARY_PATH="${nightly-rustfmt}/lib''${DYLD_LIBRARY_PATH:+:$DYLD_LIBRARY_PATH}"
              menu
            ''
            + pkgs.lib.optionalString pkgs.stdenv.isLinux ''
              export OPENSSL_NO_VENDOR=1
              export OPENSSL_LIB_DIR=${pkgs.openssl.out}/lib
              export OPENSSL_INCLUDE_DIR=${pkgs.openssl.dev}/include
            '';
        };

        formatter = pkgs.alejandra;
      }
    );
}
