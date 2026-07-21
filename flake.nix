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
            buildInputs = runtimeLibs;
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
            LD_LIBRARY_PATH = lib.makeLibraryPath [
              pkgs.openssl
              pkgs.python3
              pkgs.stdenv.cc.cc.lib
              pkgs.zlib
            ];

            shellHook = ''
              # Use the repo's shared git hooks (.githooks) so the gitleaks
              # pre-commit secret blocker is enabled for every contributor.
              git config core.hooksPath .githooks
              strip_fake_output_rpath() {
                local name="$1"
                local value="''${!name}"
                value=$(${pkgs.gnused}/bin/sed 's|-rpath [^ ]*outputs/out/lib||g' <<< "$value")
                value="-rpath ${pkgs.openssl}/lib -rpath ${pkgs.python3}/lib -rpath ${pkgs.stdenv.cc.cc.lib}/lib -rpath ${pkgs.zlib}/lib $value"
                export "$name=$value"
              }
              strip_fake_output_rpath NIX_LDFLAGS
              strip_fake_output_rpath NIX_LDFLAGS_FOR_BUILD
            '';
          };
        }
      );

      formatter = forEachSystem (_: pkgs: pkgs.nixfmt-rfc-style);
    };
}
