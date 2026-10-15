{
  description = "casper, and its Rust development shell";

  nixConfig = {
    extra-substituters = [ "https://ai-nerv.cachix.org" ];
    extra-trusted-public-keys = [
      "ai-nerv.cachix.org-1:1Wycl/383z79Nvh7ELU2jlycVzOqJyy1PGajS9caDBY="
    ];
  };

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs?rev=4c1018dae018162ec878d42fec712642d214fdfa";
    flake-utils.url = "github:numtide/flake-utils";
    nixgl.url = "github:nix-community/nixGL";
    # A pinned toolchain, as the workspace siblings have. `pkgs.rustc` is whatever nixpkgs ships
    # that day, and this repository builds under `-Dwarnings`, where one new lint is a failed
    # verify — which is how CI here went red for a week on lints nobody had written code for.
    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    { nixpkgs, flake-utils, nixgl, rust-overlay, ... }:
    let
      manifest = builtins.fromTOML (builtins.readFile ./Cargo.toml);

      # The static binary, with the shipped configuration beside it.
      packaged = flake-utils.lib.eachSystem [ "x86_64-linux" "aarch64-linux" ] (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          lib = pkgs.lib;
          # rustc 1.95, the floor monty sets, for the static target: this nixpkgs ships 1.94.
          toolchain =
            (rust-overlay.lib.mkRustBin { } pkgs.pkgsStatic.buildPackages).stable."1.95.0".minimal;
          rustPlatform = pkgs.pkgsStatic.makeRustPlatform {
            cargo = toolchain;
            rustc = toolchain;
          };
          casper = rustPlatform.buildRustPackage {
            pname = "casper";
            inherit (manifest.package) version;
            src = lib.fileset.toSource {
              root = ./.;
              fileset = lib.fileset.unions [
                ./Cargo.toml
                ./Cargo.lock
                ./build.rs
                ./grammars.lock
                ./src
                ./config
              ];
            };

            cargoLock = {
              lockFile = ./Cargo.lock;
              outputHashes = {
                "luna-0.5.0" = "sha256-YJsEKm7UykhqNQjhLPBgpmCHQZQhDLFVAENSD43SyAw=";
                "monty-0.0.23" = "sha256-NUhHd3x0hA+qpQ7wQ30kiynn4M+5XWQrX/fG2zgnlwg=";
              };
            };
            cargoBuildFlags = [
              "--bin"
              "casper"
            ];
            stripAllList = [ "bin" ];

            doCheck = false;
            # Source paths baked into the binary name /build, wherever the build ran.
            preBuild = ''
              export RUSTFLAGS="''${RUSTFLAGS:-} --remap-path-prefix=$NIX_BUILD_TOP=/build"
              export NIX_CFLAGS_COMPILE="''${NIX_CFLAGS_COMPILE:-} -ffile-prefix-map=$NIX_BUILD_TOP=/build"
            '';
            postInstall = ''
              mkdir -p "$out/share/casper"
              cp -r config "$out/share/casper/"
            '';

            doInstallCheck = true;
            nativeInstallCheckInputs = [ pkgs.binutils ];
            installCheckPhase = ''
              runHook preInstallCheck

              binary="$out/bin/casper"
              home="$TMPDIR/home"
              mkdir -p "$home/run" "$home/config"
              chmod 700 "$home/run"
              cp -r "$out/share/casper/config" "$home/config/casper"
              chmod -R u+w "$home/config"
              isolated() {
                env -i PATH="$PATH" HOME="$home" \
                  XDG_CONFIG_HOME="$home/config" XDG_DATA_HOME="$home/data" \
                  XDG_CACHE_HOME="$home/cache" XDG_STATE_HOME="$home/state" \
                  XDG_RUNTIME_DIR="$home/run" \
                  "$binary" "$@"
              }
              isolated verbs --json > "$TMPDIR/verbs.json"
              grep -q '^{"ok":true,"family":1,' "$TMPDIR/verbs.json"
              grep -q '"verb":"serve"' "$TMPDIR/verbs.json"
              isolated tools --json > "$TMPDIR/tools.json"
              grep -q '^{"ok":true,"family":1,' "$TMPDIR/tools.json"
              grep -q '"name":"read"' "$TMPDIR/tools.json"
              test -s "$out/share/casper/config/clients/hexe.lua"
              test "$(ls "$out/bin")" = casper

              # `code` names a grammar nobody fetched, and the command that fetches it.
              printf 'fn main() {}\n' > "$TMPDIR/smoke.rs"
              isolated code "$TMPDIR/smoke.rs" > "$TMPDIR/unbuilt.json"
              grep -q 'no rust grammar yet: run `casper grammar fetch rust`' "$TMPDIR/unbuilt.json"

              if readelf -l "$binary" | grep -q 'program interpreter'; then
                echo "error: $binary requests a dynamic loader" >&2
                exit 1
              fi
              if readelf -d "$binary" 2>/dev/null | grep -q NEEDED; then
                echo "error: $binary has dynamic dependencies" >&2
                exit 1
              fi

              runHook postInstallCheck
            '';

            meta = {
              description = manifest.package.description;
              homepage = "https://github.com/ai-nerv/casper";
              license = lib.licenses.mit;
              mainProgram = "casper";
              platforms = [
                "x86_64-linux"
                "aarch64-linux"
              ];
            };
          };
          app = {
            type = "app";
            program = "${casper}/bin/casper";
            meta.description = "Run casper";
          };
        in
        {
          packages = {
            default = casper;
            inherit casper;
          };
          apps = {
            default = app;
            casper = app;
          };
          checks = {
            inherit casper;
          };
        }
      );
    in
    nixpkgs.lib.recursiveUpdate packaged
    (flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [
          (import rust-overlay)
          (final: prev: {
            xorg = prev.xorg // {
              libX11 = final.libx11;
              libxcb = final.libxcb;
              libxshmfence = final.libxshmfence;
            };
          })
        ];

        pkgs = import nixpkgs {
          inherit system overlays;
          config = {
            allowUnfree = true;
            nvidia.acceptLicense = true;
          };
        };

        nvidiaVersion = builtins.getEnv "NVIDIA_VERSION";
        hasNvidia = nvidiaVersion != "";

        nixglPkgs = import "${nixgl}/default.nix" ({
          inherit pkgs;
        } // pkgs.lib.optionalAttrs hasNvidia {
          inherit nvidiaVersion;
          nvidiaHash = null;
        });

        nixGLTarget =
          if hasNvidia
          then "${nixglPkgs.nixGLNvidia}/bin/nixGLNvidia-${nvidiaVersion}"
          else "${nixglPkgs.nixGLIntel}/bin/nixGLIntel";
        nixVulkanTarget =
          if hasNvidia
          then "${nixglPkgs.nixVulkanNvidia}/bin/nixVulkanNvidia-${nvidiaVersion}"
          else "${nixglPkgs.nixVulkanIntel}/bin/nixVulkanIntel";

        nixGLAlias = pkgs.runCommand "nixGL" { } ''
          mkdir -p $out/bin
          ln -s ${nixGLTarget} $out/bin/nixGL
        '';
        nixVulkanAlias = pkgs.runCommand "nixVulkan" { } ''
          mkdir -p $out/bin
          ln -s ${nixVulkanTarget} $out/bin/nixVulkan
        '';

        guiLibs = with pkgs; [
          alsa-lib
          udev
          vulkan-loader
          libxkbcommon
          wayland
          libx11
          libxcursor
          libxi
          libxrandr
        ];
        # One release past what the workspace siblings pin: monty, behind `execute`, needs 1.95.
        rust = pkgs.rust-bin.stable."1.95.0".default.override {
          extensions = [ "rust-src" ];
        };
      in
      {
        devShells.default = pkgs.mkShell {
          packages = [
            rust
            pkgs.rust-analyzer
            pkgs.git-cliff
            # Reads every manifest against its own sources, so a dependency nothing uses is a
            # failure rather than a line somebody eventually notices. See the note in
            # `Cargo.toml` for why this and not `unused_crate_dependencies`.
            pkgs.cargo-machete
            # The dependency policy in `deny.toml`, and the advisory database against the lockfile.
            pkgs.cargo-deny
            pkgs.cargo-audit
            pkgs.clang
            pkgs.mold
            pkgs.pkg-config

          # Graphics wrappers and the libraries a windowed program needs, on x86_64 only.
          #
          # nixGL pulls `pkgsi686Linux` for its 32-bit loaders, which does not exist on aarch64 —
          # so a shell that asked for it unconditionally could not be *evaluated* there at all,
          # let alone entered. That made `nix develop` fail on the arm64 release runner with
          # "i686 Linux package set can only be used with the x86 family", which took the amd64
          # leg down with it and produced no binary for either.
          #
          # None of it is needed to build: this is for running something that draws, on a
          # machine whose driver needs wrapping.
          ] ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isx86_64 ([
            nixGLAlias
            nixVulkanAlias
            nixglPkgs.nixGLIntel
            nixglPkgs.nixVulkanIntel
          ] ++ pkgs.lib.optionals hasNvidia [
            nixglPkgs.nixGLNvidia
            nixglPkgs.nixVulkanNvidia
          ] ++ guiLibs);

          # A musl toolchain for the static build, handed over as a path rather than a package.
          # As a package its headers land on the default search path, and an ordinary build then
          # compiles against musl while linking against glibc -- which succeeds without a word and
          # crashes at startup. Only the static build is given it: .make.lua reads MUSL_CC.
          # gcc targeting musl, which is the only one of the two that has a C++ standard library.
          MUSL_CC = pkgs.pkgsMusl.stdenv.cc;
          # musl-clang: the host clang, pointed at musl's headers and libs. C only -- it has no
          # libstdc++, so a C++ build against it fails on the first #include <string>.
          MUSL_CLANG = pkgs.musl.dev;

          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath guiLibs;
          WGPU_VALIDATION = "0";
          WGPU_DEBUG = "0";
        };
      }
    ));
}
