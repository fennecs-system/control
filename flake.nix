{
  description = "Control Rust development environment";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  outputs = { self, nixpkgs }:
    let
      systems = [ "aarch64-darwin" "x86_64-darwin" "aarch64-linux" "x86_64-linux" ];
      eachSystem = f: nixpkgs.lib.genAttrs systems (system: f (import nixpkgs {
        inherit system;
        config.allowUnfreePredicate = pkg: nixpkgs.lib.getName pkg == "claude-code";
      }));
    in {
      devShells = eachSystem (pkgs: {
        default = pkgs.mkShell {
          packages = [ pkgs.rustc pkgs.cargo pkgs.rustfmt pkgs.clippy pkgs.lld pkgs.trunk pkgs.ripgrep pkgs.fd ];
        };
      });
      packages = eachSystem (pkgs:
        if pkgs.stdenv.hostPlatform.isLinux then {
          worker-image = import ./examples/worker-image.nix { inherit pkgs; };
          claude-image = import ./examples/worker-image.nix { inherit pkgs; backend = "claude"; };
          browser-image = import ./tools/browser/image.nix { inherit pkgs; };
        } else { });
    };
}
