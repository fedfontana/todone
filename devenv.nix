{
  pkgs,
  lib,
  config,
  inputs,
  ...
}:

{
  packages = with pkgs; [
    # Forge backend used by `todone port` (GitHub issues via the gh CLI).
    gh
    # Coverage measurement for the test suite.
    cargo-llvm-cov
    # Default $EDITOR for draft/read-only sessions when no editor is set.
    neovim
  ];

  languages.rust.enable = true;

  pre-commit.enable = true;
  pre-commit.hooks = {
    cargo-fmt = {
      enable = true;
      settings.packageFeatures.workspace = true;
    };
    cargo-clippy = {
      enable = true;
      settings.clippyArgs = [
        "--all-targets"
        "--all-features"
        "--"
        "-D"
        "warnings"
      ];
    };
    cargo-test = {
      enable = true;
      settings.cargoTestArgs = [ "--all" ];
    };
  };
}
