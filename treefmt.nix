{ pkgs, ... }:
{
  projectRootFile = "flake.nix";

  programs = {
    nixpkgs-fmt.enable = true;
    statix.enable = true;
    rustfmt.enable = true;
    taplo.enable = true;
    shellcheck.enable = true;
  };
}
