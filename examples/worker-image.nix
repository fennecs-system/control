# Build on a Linux Nix builder. This supplies the worker, not the control plane.
{ pkgs, backend ? "codex" }:
let
  tcpRelay = pkgs.runCommand "control-tcp-relay" { nativeBuildInputs = [ pkgs.rustc pkgs.stdenv.cc ]; } ''
    mkdir -p $out/bin
    cp ${../tools/tcp-relay/main.rs} main.rs
    cp ${../tools/tcp-relay/protocol.rs} protocol.rs
    rustc --edition=2024 -Dwarnings -O main.rs -o $out/bin/control-tcp-relay
  '';
  serviceRunner = pkgs.runCommand "control-service-runner" { nativeBuildInputs = [ pkgs.rustc pkgs.stdenv.cc ]; } ''
    mkdir -p $out/bin
    rustc --edition=2024 -O ${../tools/service-runner/main.rs} -o $out/bin/control-service-runner
  '';
  playground = pkgs.runCommand "control-playground" { nativeBuildInputs = [ pkgs.rustc pkgs.stdenv.cc ]; } ''
    mkdir -p $out/bin src/playground
    cp ${../tools/playground/main.rs} src/playground/main.rs
    cp ${../tools/platform-flags.rs} src/platform-flags.rs
    rustc --edition=2024 -O src/playground/main.rs -o $out/bin/control-playground
  '';
  roomMcp = pkgs.runCommand "control-room-mcp" { nativeBuildInputs = [ pkgs.rustc pkgs.stdenv.cc ]; } ''
    mkdir -p $out/bin src/room-mcp
    cp ${../tools/room-mcp/main.rs} src/room-mcp/main.rs
    cp ${../tools/platform-flags.rs} src/platform-flags.rs
    rustc --edition=2024 -O src/room-mcp/main.rs -o $out/bin/control-room-mcp
  '';
in
pkgs.dockerTools.buildLayeredImage {
  name = if backend == "claude" then "control-claude" else "control-worker";
  tag = "nix";
  contents = [
    roomMcp playground serviceRunner tcpRelay
    (if backend == "claude" then pkgs.claude-code else pkgs.codex)
    pkgs.rustc pkgs.cargo pkgs.rustfmt pkgs.clippy
    pkgs.gitMinimal pkgs.stdenv.cc pkgs.pkg-config pkgs.ripgrep pkgs.fd
    pkgs.bash pkgs.coreutils pkgs.cacert
  ];
  extraCommands = ''
    mkdir -p tmp home/agent workspace
    chmod 1777 tmp
    chmod 0777 home/agent workspace
  '';
  config = {
    Labels = { "dev.control.service-runner" = "1"; "dev.control.tcp-relay" = "2"; };
    WorkingDir = "/workspace";
    Env = [
      "HOME=/home/agent"
      "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
      "PATH=/bin"
    ];
    Cmd = [ (if backend == "claude" then "claude" else "codex") "--version" ];
  };
}
