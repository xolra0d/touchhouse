{
  description = "A very basic flake";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs?ref=nixos-unstable";
  };

  outputs = { self, nixpkgs }:
  let
    system = "x86_64-linux";
    pkgs = nixpkgs.legacyPackages.${system};
  in {
    devShells.${system}.default = pkgs.mkShell {
      # if need want to add crates, that provide bindings
      # to external libs, e.g. for `glib` or `tch`, then
      # 1. Add lib to buildInputs list
      # 2. add after buildInputs: `nativeBuildInputs = [ pkgs.pkg-config ];` line
      buildInputs = with pkgs; [
        cargo rustc rustfmt clippy rust-analyzer 
      ];
      env.RUST_SRC_PATH = "${pkgs.rust.packages.stable.rustPlatform.rustLibSrc}";
    };

    # for nix packages look: https://www.youtube.com/watch?v=Ss1IXtYnpsg
  };
}
