# Build on a Linux Nix builder. This supplies the worker, not the control plane.
{ pkgs }:
pkgs.dockerTools.buildLayeredImage {
  name = "control-worker";
  tag = "nix";
  contents = [
    pkgs.codex pkgs.rustc pkgs.cargo pkgs.rustfmt pkgs.clippy
    pkgs.gitMinimal pkgs.stdenv.cc pkgs.pkg-config
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
