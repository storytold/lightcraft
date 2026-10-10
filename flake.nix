{
  description = "Photo library and non-destructive raw developer, in pure Rust";

  inputs = {
    # The app needs edition 2024 and rustc ≥ 1.90, so the flake tracks unstable. Consumers on a
    # release channel can retarget it: `inputs.<this-flake>.inputs.nixpkgs.follows = "nixpkgs";`
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    # Optional build input of the repository: the CJK fonts (BIZ UDPGothic, BIZ UDMincho, Noto Sans CJK SC,
    # Shippori Mincho) that `crates/engine-export/build.rs` embeds when `CRAFT_FONTS_DIR` is set. Official
    # releases always build with them; without them everything works but Japanese text has no
    # glyphs. Pinned to the revision the release workflow uses — bump deliberately
    # (`nix flake update craft-fonts`); craftrules standards/fonts.md.
    craft-fonts = {
      url = "github:storytold/craft-fonts/abb83316d96aa59c1cf64784289e378fe9fa5695";
      flake = false;
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      craft-fonts,
    }:
    let
      inherit (nixpkgs) lib;

      # No x86_64-darwin: the pinned nixpkgs (26.11, "nixos-unstable") dropped support for it. On
      # an Intel Mac, retarget nixpkgs at a release that still has it, or install the .dmg from the
      # GitHub release.
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];

      # The AppStream metadata carries a release date. The flake source's own commit date is
      # reproducible, unlike "now"; a tarball or dirty tree without one falls back to the epoch.
      buildDate =
        let
          d = self.lastModifiedDate or "";
        in
        if builtins.stringLength d >= 8 then
          "${builtins.substring 0 4 d}-${builtins.substring 4 2 d}-${builtins.substring 6 2 d}"
        else
          "1970-01-01";

      packageArgs = {
        inherit craft-fonts buildDate;
      };

      forAllSystems = f: lib.genAttrs systems (system: f (import nixpkgs { inherit system; }) system);
    in
    {
      packages = forAllSystems (
        pkgs: _:
        let
          app = pkgs.callPackage ./nix/package.nix packageArgs;
        in
        {
          inherit app;
          default = app;
        }
      );

      # `nixpkgs.overlays = [ inputs.<this-flake>.overlays.default ]` → `pkgs.app` everywhere.
      overlays.default = final: _prev: {
        app = final.callPackage ./nix/package.nix packageArgs;
      };

      devShells = forAllSystems (
        pkgs: system:
        let
          app = self.packages.${system}.app;
        in
        {
          default = pkgs.mkShell (
            {
              # The package's own native inputs, so `cargo build` in the shell needs no extra setup.
              inputsFrom = [ app ];
              packages = with pkgs; [
                cargo
                rustc
                clippy
                rustfmt
                rust-analyzer
              ];
              # `cargo run` dlopens the Vulkan loader and libxkbcommon like the packaged binary.
              LD_LIBRARY_PATH = lib.makeLibraryPath app.passthru.runtimeLibs;
              RUST_BACKTRACE = "1";
            }
            // lib.optionalAttrs (craft-fonts != null) {
              # CJK glyphs in `cargo run` too; build.rs reads this as CRAFT_FONTS_DIR.
              CRAFT_FONTS_DIR = toString craft-fonts;
            }
          );
        }
      );

      checks = forAllSystems (
        pkgs: system: {
          app = self.packages.${system}.app;

          # `nix fmt` formats the flake; this keeps it formatted.
          formatting = pkgs.runCommand "app-nix-formatting" { nativeBuildInputs = [ pkgs.nixfmt ]; } ''
            nixfmt --check ${./flake.nix} ${./nix}
            touch $out
          '';
        }
      );

      formatter = forAllSystems (pkgs: _: pkgs.nixfmt);
    };
}
