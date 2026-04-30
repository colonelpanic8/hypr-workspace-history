{
  description = "Hyprland workspace history cycling plugin";

  inputs = {
    hyprland.url = "git+https://github.com/hyprwm/Hyprland?submodules=1";
    nixpkgs.follows = "hyprland/nixpkgs";
    systems.follows = "hyprland/systems";
  };

  outputs = {
    self,
    hyprland,
    nixpkgs,
    systems,
    ...
  }: let
    inherit (nixpkgs) lib;
    eachSystem = lib.genAttrs (import systems);
    pkgsFor = eachSystem (system:
      import nixpkgs {
        localSystem.system = system;
        overlays = [hyprland.overlays.hyprland-packages];
      });
  in {
    packages = eachSystem (system: let
      pkgs = pkgsFor.${system};
      hyprlandPkg = hyprland.packages.${system}.hyprland;
    in {
      default = pkgs.hyprlandPlugins.mkHyprlandPlugin {
        pluginName = "hypr-workspace-history";
        version = "0.1.0";
        src = builtins.path {
          path = ./.;
          name = "hypr-workspace-history-source";
        };

        inherit (hyprlandPkg) nativeBuildInputs;

        meta = {
          description = "Workspace history cycling plugin for Hyprland";
          homepage = "https://github.com/colonelpanic8/hypr-workspace-history";
          license = lib.licenses.bsd3;
          platforms = lib.platforms.linux;
        };
      };

      hypr-workspace-history = self.packages.${system}.default;
    });

    checks = eachSystem (system: {
      hypr-workspace-history = self.packages.${system}.default;
    });

    devShells = eachSystem (system: let
      pkgs = pkgsFor.${system};
      hyprlandPkg = hyprland.packages.${system}.hyprland;
    in {
      default = pkgs.mkShell.override {stdenv = pkgs.gcc15Stdenv;} {
        name = "hypr-workspace-history";
        buildInputs = [hyprlandPkg];
        inputsFrom = [hyprlandPkg self.packages.${system}.default];
      };
    });
  };
}
