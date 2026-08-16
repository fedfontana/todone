{
  pkgs,
  lib,
  config,
  inputs,
  ...
}:

{
  packages = with pkgs; [
    # VCS (also required by the git-hooks pre-commit integration).
    git
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
    rustfmt = {
      enable = true;
    };
    clippy = {
      enable = true;
      settings = {
        denyWarnings = true;
        allFeatures = true;
        extraArgs = "--all-targets";
      };
    };
    cargo-test = {
      enable = true;
      name = "cargo test";
      entry = "cargo test --all";
      pass_filenames = false;
    };
  };
}
