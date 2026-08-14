{
  description = "pulpit — the PDF presenter that does not screw up your projector";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };

        # winit, wgpu and PDFium are all loaded with dlopen at run time, so
        # these must be on the loader path of the *running* binary — not just
        # available at build time. This is the entire reason a plain
        # `cargo install` build fails to start on NixOS while working fine on
        # distributions with a global /usr/lib.
        runtimeLibraries = with pkgs; [
          libxkbcommon
          vulkan-loader
          wayland
          libxcursor
          libxrandr
          libxi
          libX11
          libGL
        ];

        buildTools = with pkgs; [ pkg-config makeWrapper ];

        buildLibraries = with pkgs; [ dbus ] ++ runtimeLibraries;

        # PDFium is not vendored: a pinned prebuilt copy is fetched with a
        # verified hash, exactly as scripts/fetch-pdfium.sh does outside Nix.
        # One entry per supported target, kept on the same pinned release as
        # scripts/fetch-pdfium.sh. A target with no entry fails the build here
        # rather than silently wrapping the binary with a foreign libpdfium.
        pdfiumArtifacts = {
          "x86_64-linux" = {
            artifact = "pdfium-linux-x64.tgz";
            hash = "sha256-w69YD53w/vlUW0QRW8XqRA8oaVa18jHfafs3O478T2k=";
          };
          "aarch64-linux" = {
            artifact = "pdfium-linux-arm64.tgz";
            hash = "sha256-oZhio24rLaPD+0Pw3u9F+7wzH1jNR5Q3gq5L2dtMZtk=";
          };
          "aarch64-darwin" = {
            artifact = "pdfium-mac-arm64.tgz";
            hash = "sha256-4hTuM/IrIgTap2WlRa7h5CXYhEjmFU2slcagYga3Q38=";
          };
          "x86_64-darwin" = {
            artifact = "pdfium-mac-x64.tgz";
            hash = "sha256-S5JNlI0uxIY0NdN1qUVBtAA8WfitwozF5CNrCrgaNV0=";
          };
        };

        pdfiumArtifact = pdfiumArtifacts.${system} or (throw
          "no pinned PDFium artifact for ${system}; pin one in flake.nix and scripts/fetch-pdfium.sh together");

        pdfium = pkgs.stdenv.mkDerivation {
          pname = "pdfium-binaries";
          version = "chromium-7999";
          src = pkgs.fetchurl {
            url =
              "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F7999/${pdfiumArtifact.artifact}";
            inherit (pdfiumArtifact) hash;
          };
          sourceRoot = ".";
          installPhase = ''
            mkdir -p $out/lib $out/share/licenses/pdfium
            cp lib/libpdfium.* $out/lib/
            cp LICENSE $out/share/licenses/pdfium/ || true
            cp VERSION $out/share/licenses/pdfium/ || true
          '';
          meta.license = pkgs.lib.licenses.bsd3;
        };

        pulpit = pkgs.rustPlatform.buildRustPackage {
          pname = "pulpit";
          version = "0.1.0";
          src = pkgs.lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = buildTools;
          buildInputs = buildLibraries;

          # Every worker — renderer and media alike — is a role of the
          # application's own binary, re-executed with a flag, so there is
          # nothing else to build or install here.
          postInstall = ''
            install -Dm644 packaging/pulpit.desktop \
              $out/share/applications/pulpit.desktop
            install -Dm644 packaging/pulpit.svg \
              $out/share/icons/hicolor/scalable/apps/pulpit.svg
            install -Dm644 README.md $out/share/doc/pulpit/README.md
            install -Dm644 LICENSES/README.md \
              $out/share/doc/pulpit/licenses/README.md
            install -Dm644 LICENSES/LICENSE-MIT \
              $out/share/doc/pulpit/licenses/LICENSE-MIT
            install -Dm644 LICENSES/LICENSE-APACHE \
              $out/share/doc/pulpit/licenses/LICENSE-APACHE
            install -Dm644 LICENSES/ICED_AW-LICENSE \
              $out/share/doc/pulpit/licenses/ICED_AW-LICENSE
            install -Dm644 LICENSES/LUCIDE-LICENSE \
              $out/share/doc/pulpit/licenses/LUCIDE-LICENSE

            wrapProgram $out/bin/pulpit \
              --prefix LD_LIBRARY_PATH : "${pkgs.lib.makeLibraryPath runtimeLibraries}" \
              --set-default PULPIT_PDFIUM_PATH "${pdfium}/lib"
          '';

          # The test suite spawns worker processes and touches /dev/shm, both
          # of which work in the Nix sandbox; the PDFium and Wayland tests
          # skip themselves when their prerequisites are absent.
          checkFlags = [ ];

          meta = with pkgs.lib; {
            description = "A focused PDF presenter with dependable multi-monitor behaviour";
            license = with licenses; [ mit asl20 ];
            platforms = platforms.linux;
            mainProgram = "pulpit";
          };
        };
      in {
        packages = {
          default = pulpit;
          inherit pulpit pdfium;
        };

        apps.default = flake-utils.lib.mkApp { drv = pulpit; };

        devShells.default = pkgs.mkShell {
          # nfpm builds the .deb and the .rpm from one description
          # (`make linux-packages`); rpm is not otherwise needed to develop.
          nativeBuildInputs = buildTools ++ (with pkgs; [ cargo rustc rustfmt clippy rust-analyzer nfpm ]);
          buildInputs = buildLibraries;

          # A cargo-built binary from this shell needs the same loader path
          # the wrapper bakes in for the packaged build.
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath runtimeLibraries;

          PULPIT_PDFIUM_PATH = "${pdfium}/lib";

          shellHook = ''
            echo "pulpit dev shell: cargo run -- deck.pdf"
            echo "PDFium: $PULPIT_PDFIUM_PATH"
          '';
        };
      });
}
