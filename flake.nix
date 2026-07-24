{
  description = "n00n - AI coding agent";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
    }:
    let
      lib = nixpkgs.lib;
      cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
      packageName = cargoToml.package.name;
      version = cargoToml.workspace.package.version;
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forEachSystem =
        f:
        lib.genAttrs systems (
          system:
          f system (
            import nixpkgs {
              inherit system;
              overlays = [ rust-overlay.overlays.default ];
            }
          )
        );
    in
    {
      packages = forEachSystem (
        system: pkgs:
        let
          rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          rustPlatform = pkgs.makeRustPlatform {
            cargo = rustToolchain;
            rustc = rustToolchain;
          };
          runtimeLibs = with pkgs; [
            openssl
            python3
            stdenv.cc.cc.lib
            zlib
          ];
          n00n = rustPlatform.buildRustPackage {
            pname = packageName;
            inherit version;
            src = ./.;
            cargoLock = {
              lockFile = ./Cargo.lock;
              # NOTE: these are cargo git dependencies; set hash to "" and
              # rebuild to get the correct value.
              outputHashes = {
                "monty-0.0.18" = "sha256-p9mDjS9FTvsITU98B8AeyUCk4wQhgk71HoyOsNPpB0Y=";
                "ruff_python_ast-0.0.0" = "sha256-m5U5OVUvhn5t3yTSSbT/JA+xmydEDQq+zKFNMN7K/MI=";
                "ruff_python_parser-0.0.0" = "sha256-m5U5OVUvhn5t3yTSSbT/JA+xmydEDQq+zKFNMN7K/MI=";
                "ruff_python_stdlib-0.0.0" = "sha256-m5U5OVUvhn5t3yTSSbT/JA+xmydEDQq+zKFNMN7K/MI=";
                "ruff_python_trivia-0.0.0" = "sha256-m5U5OVUvhn5t3yTSSbT/JA+xmydEDQq+zKFNMN7K/MI=";
                "ruff_source_file-0.0.0" = "sha256-m5U5OVUvhn5t3yTSSbT/JA+xmydEDQq+zKFNMN7K/MI=";
                "ruff_text_size-0.0.0" = "sha256-m5U5OVUvhn5t3yTSSbT/JA+xmydEDQq+zKFNMN7K/MI=";
              };
            };
            cargoBuildFlags = [
              "--package"
              packageName
            ];
            nativeBuildInputs = with pkgs; [
              makeWrapper
              pkg-config
              perl
              python3
            ];
            # TODO: Upstream monty includes a relative README path that doesn't
            # survive nix vendoring. Remove this once `monty` stops including
            # the relative path
            postPatch = ''
              for f in "$cargoDepsCopy"/monty-*/src/lib.rs; do
              # monty-macros doesn't include the readme
              if [[ "$f" != *"monty-macros"* ]]; then
                substituteInPlace "$f" \
                  --replace-fail '#![doc = include_str!("../../../README.md")]' \
                                 '#![doc = "Monty Python bridge."]'
              fi
              done
            '';
            buildInputs = with pkgs; [ openssl stdenv.cc.cc.lib ];
            doCheck = false;

            postInstall = ''
              wrapProgram $out/bin/n00n \
                --prefix LD_LIBRARY_PATH : "${lib.makeLibraryPath runtimeLibs}"
            '';
          };
        in
        {
          default = n00n;
        }
      );

      devShells = forEachSystem (
        _: pkgs:
        let
          certs = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
          rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              rustToolchain
              cargo-nextest
              git
              gitleaks
              just
              openssl
              perl
              pkg-config
              python3
              ripgrep
              ruff
              stylua
              ty
            ];

            RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
            SSL_CERT_FILE = certs;
            NIX_SSL_CERT_FILE = certs;
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [
              pkgs.openssl
              pkgs.stdenv.cc.cc.lib
            ];
          };
        }
      );

      formatter = forEachSystem (_: pkgs: pkgs.nixfmt-rfc-style);
    };
}
