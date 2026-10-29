# Build on a Linux Nix builder. This supplies the worker, not the control plane.
{ pkgs }:
let
  playground = pkgs.runCommand "control-playground" { nativeBuildInputs = [ pkgs.rustc pkgs.stdenv.cc ]; } ''
    mkdir -p $out/bin
    rustc --edition=2021 -O ${../tools/playground/main.rs} -o $out/bin/control-playground
  '';
  roomMcp = pkgs.runCommand "control-room-mcp" { nativeBuildInputs = [ pkgs.rustc pkgs.stdenv.cc ]; } ''
    mkdir -p $out/bin
    rustc --edition=2021 -O ${../tools/room-mcp/main.rs} -o $out/bin/control-room-mcp
  '';
in
pkgs.dockerTools.buildLayeredImage {
  name = "control-worker";
  tag = "nix";
  contents = [
    roomMcp playground
    pkgs.codex pkgs.rustc pkgs.cargo pkgs.rustfmt pkgs.clippy
    pkgs.gitMinimal pkgs.stdenv.cc pkgs.pkg-config pkgs.ripgrep pkgs.fd
    pkgs.bash pkgs.coreutils pkgs.cacert
  ];
  extraCommands = ''
    mkdir -p tmp home/agent workspace
    chmod 1777 tmp
    chmod 0777 home/agent workspace
  '';
  config = {
    WorkingDir = "/workspace";
    Env = [
      "HOME=/home/agent"
      "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
      "PATH=/bin"
    ];
    Cmd = [ "codex" "--version" ];
  };
}
