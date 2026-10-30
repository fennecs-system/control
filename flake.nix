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
      apps = eachSystem (pkgs:
        let
          linuxSystem = builtins.replaceStrings [ "darwin" ] [ "linux" ] pkgs.stdenv.hostPlatform.system;
          setup = pkgs.writeShellApplication {
            name = "control-setup";
            # stdenv.cc supplies the C compiler native crates (ring) need; Xcode CLT is not required.
            runtimeInputs = [ pkgs.rustc pkgs.cargo pkgs.lld pkgs.trunk pkgs.gitMinimal pkgs.coreutils pkgs.stdenv.cc ];
            # runtimeInputs only extends PATH; Rust's Darwin std links -liconv, which stdenv
            # hooks would otherwise put on the linker search path.
            text = nixpkgs.lib.optionalString pkgs.stdenv.hostPlatform.isDarwin ''
              export LIBRARY_PATH=${pkgs.lib.getLib pkgs.libiconv}/lib''${LIBRARY_PATH:+:$LIBRARY_PATH}
            '' + ''
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
        if pkgs.stdenv.hostPlatform.isLinux then {
          worker-image = import ./examples/worker-image.nix { inherit pkgs; };
          claude-image = import ./examples/worker-image.nix { inherit pkgs; backend = "claude"; };
          browser-image = import ./tools/browser/image.nix { inherit pkgs; };
          build-image = import ./tools/build/image.nix { inherit pkgs; };
        } else { });
    };
}
