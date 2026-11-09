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
          packages = [ pkgs.rustc pkgs.cargo pkgs.rustfmt pkgs.clippy pkgs.lld pkgs.trunk pkgs.wasm-bindgen-cli_0_2_129 pkgs.ripgrep pkgs.fd ];
        };
      });
      apps = eachSystem (pkgs:
        let
          linuxSystem = builtins.replaceStrings [ "darwin" ] [ "linux" ] pkgs.stdenv.hostPlatform.system;
          setup = pkgs.writeShellApplication {
            name = "control-setup";
            runtimeInputs = [ pkgs.nix pkgs.gitMinimal pkgs.coreutils ];
            text = ''
              export CONTROL_SETUP_LINUX_SYSTEM=${linuxSystem}
              ${builtins.readFile ./hosting/setup.sh}
            '';
          };
        in {
          setup = { type = "app"; program = "${setup}/bin/control-setup"; };
          codex = { type = "app"; program = "${pkgs.codex}/bin/codex"; };
        } // nixpkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isDarwin {
          builder = { type = "app"; program = "${pkgs.darwin.linux-builder}/bin/create-builder"; };
        });
      packages = eachSystem (pkgs:
        let control = import ./hosting/package.nix { inherit pkgs; }; in {
          inherit control;
          default = control;
        } // nixpkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
          worker-image = import ./examples/worker-image.nix { inherit pkgs; };
          worker-full-image = import ./examples/worker-image.nix { inherit pkgs; profile = "full"; };
          claude-image = import ./examples/worker-image.nix { inherit pkgs; backend = "claude"; };
          claude-full-image = import ./examples/worker-image.nix { inherit pkgs; backend = "claude"; profile = "full"; };
          browser-image = import ./tools/browser/image.nix { inherit pkgs; };
          build-image = import ./tools/build/image.nix { inherit pkgs; };
        });
    };
}
