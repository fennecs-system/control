{
  description = "Control Rust development environment";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  outputs = { self, nixpkgs }:
    let
      systems = [ "aarch64-darwin" "x86_64-darwin" "aarch64-linux" "x86_64-linux" ];
      eachSystem = f: nixpkgs.lib.genAttrs systems (system: f (import nixpkgs {
        inherit system;
        overlays = [ (import ./tools/inference/llama.nix) ];
        # CUDA libraries are only evaluated for the optional inference-image-cuda output.
        config.allowUnfreePredicate = pkg:
          let
            lib = nixpkgs.lib;
            nvidia = license: lib.hasInfix "CUDA" (license.fullName or "") || lib.hasInfix "NVIDIA" (license.fullName or "");
          in
          lib.getName pkg == "claude-code" || lib.any nvidia (lib.toList (pkg.meta.license or [ ]));
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
          # Platform-selected local llama.cpp service; see docs/LOCAL-MODELS.md.
          inference = pkgs.writeShellApplication {
            name = "control-inference";
            runtimeInputs = [ pkgs.nix pkgs.coreutils pkgs.gnused pkgs.gnugrep pkgs.curl ];
            text = ''
              export CONTROL_INFERENCE_LINUX_SYSTEM=${linuxSystem}
              export CONTROL_LLAMA_SERVER=${if pkgs.stdenv.hostPlatform.isDarwin then "${pkgs.llama-cpp}" else ""}
              ${builtins.readFile ./hosting/inference.sh}
            '';
          };
        in {
          setup = { type = "app"; program = "${setup}/bin/control-setup"; };
          inference = { type = "app"; program = "${inference}/bin/control-inference"; };
          codex = { type = "app"; program = "${pkgs.codex}/bin/codex"; };
        } // nixpkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isDarwin {
          # 8 GiB: the stock 3 GiB VM cannot compile large Rust crates (Grok Build's shell crate).
          builder = { type = "app"; program = "${pkgs.darwin.linux-builder.override { modules = [ { virtualisation.darwin-builder.memorySize = 8192; } ]; }}/bin/create-builder"; };
        });
      packages = eachSystem (pkgs:
        let
          revision = self.rev or (self.dirtyRev or null);
          control = import ./hosting/package.nix { inherit pkgs revision; };
        in {
          inherit control;
          control-server = import ./hosting/package.nix { inherit pkgs revision; withTerminal = false; };
          code-metrics = import ./tools/code-metrics/package.nix { inherit pkgs; };
          default = control;
        } // nixpkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
          worker-image = import ./examples/worker-image.nix { inherit pkgs; };
          worker-full-image = import ./examples/worker-image.nix { inherit pkgs; profile = "full"; };
          claude-image = import ./examples/worker-image.nix { inherit pkgs; backend = "claude"; };
          claude-full-image = import ./examples/worker-image.nix { inherit pkgs; backend = "claude"; profile = "full"; };
          grok-image = import ./examples/worker-image.nix { inherit pkgs; backend = "grok"; };
          grok-full-image = import ./examples/worker-image.nix { inherit pkgs; backend = "grok"; profile = "full"; };
          browser-image = import ./tools/browser/image.nix { inherit pkgs; };
          build-image = import ./tools/build/image.nix { inherit pkgs; };
          inference-image = import ./tools/inference/image.nix { inherit pkgs; };
          solver-image = import ./tools/solver/image.nix { inherit pkgs; };
          inference-image-vulkan = import ./tools/inference/image.nix { inherit pkgs; acceleration = "vulkan"; };
          inference-image-cuda = import ./tools/inference/image.nix { inherit pkgs; acceleration = "cuda"; };
        });
    };
}
