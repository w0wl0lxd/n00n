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
          runtimeLibraryPath = lib.makeLibraryPath runtimeLibs;
          loaderLibraryPathVar = if pkgs.stdenv.isDarwin then "DYLD_LIBRARY_PATH" else "LD_LIBRARY_PATH";
          n00n = rustPlatform.buildRustPackage {
            pname = packageName;
            inherit version;
            src = ./.;
            cargoLock = {
              lockFile = ./Cargo.lock;
              # NOTE: these are cargo git dependencies; set hash to "" and
              # rebuild to get the correct value.
              outputHashes = {
                "monty-0.0.21" = "sha256-P4PgqfYykkZrWGg5G3WQo070lORLEhmXQUQPx3+Yslo=";
              };
            };
            cargoBuildFlags = [
              "--package"
              packageName
            ];
            nativeBuildInputs =
              with pkgs;
              [
                pkg-config
                perl
                python3
              ]
              ++ lib.optionals stdenv.isLinux [ patchelf ]
              ++ lib.optionals (!stdenv.isLinux) [ makeWrapper ];
            buildInputs = with pkgs; [
              openssl
              stdenv.cc.cc.lib
            ];
            doCheck = false;

            postFixup =
              lib.optionalString pkgs.stdenv.isLinux ''
                old_rpath="$(patchelf --print-rpath "$out/bin/${packageName}")"
                new_rpath="${runtimeLibraryPath}''${old_rpath:+:$old_rpath}"
                patchelf --set-rpath "$new_rpath" "$out/bin/${packageName}"
              ''
              + lib.optionalString (!pkgs.stdenv.isLinux) ''
                wrapProgram "$out/bin/${packageName}" \
                  --prefix ${loaderLibraryPathVar} : "${runtimeLibraryPath}"
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
