{
  description = "LightCraft — photo library and non-destructive raw developer, in pure Rust";

  inputs = {
    # LightCraft needs edition 2024 and rustc ≥ 1.95, so the flake tracks unstable. Consumers on a
    # release channel can retarget it: `inputs.lightcraft.inputs.nixpkgs.follows = "nixpkgs";`
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    # Optional build input of the repository: the default fonts (BIZ UDPGothic Regular/Bold, BIZ UDMincho,
    # Shippori Mincho, Noto Sans CJK SC, Noto Sans Arabic, Noto Sans Thai, Noto Sans Georgian,
    # NanumGothic, Noto Sans Symbols, Noto Sans Symbols 2, Noto Sans Hebrew and Noto Sans Devanagari)
    # listed in craft-fonts' `fonts/manifest.txt`, which `crates/engine/build.rs` embeds when
    # `CRAFT_FONTS_DIR` is set. The opt-in LXGW WenKai Lite face in `fonts/simplified-chinese-manifest.txt`
    # is not part of that default set and is not embedded. Official releases always build with the
    # default fonts; without them everything works but non-Latin text lacks glyphs. Pinned to the same
    # revision CI, the release workflow and the standalone packaging helpers use — bump all of them
    # together (`nix flake update craft-fonts`); craftrules standards/fonts.md.
    craft-fonts = {
      url = "github:storytold/craft-fonts/b076417b5880bd4154bd22f00695b1d28c6bea83";
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
          lightcraft = pkgs.callPackage ./nix/package.nix packageArgs;
        in
        {
          inherit lightcraft;
          default = lightcraft;
        }
      );

      # `nixpkgs.overlays = [ inputs.lightcraft.overlays.default ]` → `pkgs.lightcraft` everywhere.
      overlays.default = final: _prev: {
        lightcraft = final.callPackage ./nix/package.nix packageArgs;
      };

      devShells = forAllSystems (
        pkgs: system:
        let
          lightcraft = self.packages.${system}.lightcraft;
        in
        {
          default = pkgs.mkShell (
            {
              # The package's own native inputs, so `cargo build` in the shell needs no extra setup.
              inputsFrom = [ lightcraft ];
              packages = with pkgs; [
                cargo
                rustc
                clippy
                rustfmt
                rust-analyzer
              ];
              # `cargo run` dlopens the Vulkan loader and libxkbcommon like the packaged binary.
              LD_LIBRARY_PATH = lib.makeLibraryPath lightcraft.passthru.runtimeLibs;
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
          lightcraft = self.packages.${system}.lightcraft;

          # `nix fmt` formats the flake; this keeps it formatted.
          formatting = pkgs.runCommand "lightcraft-nix-formatting" { nativeBuildInputs = [ pkgs.nixfmt ]; } ''
            nixfmt --check ${./flake.nix} ${./nix}
            touch $out
          '';
        }
      );

      formatter = forAllSystems (pkgs: _: pkgs.nixfmt);
    };
}
