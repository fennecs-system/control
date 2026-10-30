# Linux-only image; no distribution base, network access, or credentials required.
{ pkgs }:
let
  runner = pkgs.runCommand "control-analyze" { nativeBuildInputs = [ pkgs.rustc pkgs.stdenv.cc ]; } ''
    mkdir -p $out/bin
    rustc --edition=2024 -C opt-level=2 ${./main.rs} -o $out/bin/control-analyze
  '';
in pkgs.dockerTools.buildLayeredImage {
  name = "control-analysis";
  tag = "nix";
  contents = [ runner pkgs.rustc pkgs.cargo pkgs.rustfmt pkgs.clippy pkgs.stdenv.cc ];
  extraCommands = ''
    mkdir -p source out tmp home/tool
    chmod 1777 tmp
    chmod 0777 home/tool
  '';
  config = {
    WorkingDir = "/source";
    Env = [ "PATH=/bin" "HOME=/home/tool" "CARGO_HOME=/home/tool/.cargo" "CARGO_TARGET_DIR=/out/target" ];
    Entrypoint = [ "${runner}/bin/control-analyze" "--inside" ];
    Cmd = [ "fmt" ];
  };
}
