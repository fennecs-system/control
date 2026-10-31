# Build on a Linux Nix builder. This supplies the worker, not the control plane.
{ pkgs, backend ? "codex", profile ? "lite" }:
assert builtins.elem profile [ "lite" "full" ];
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
  tag = if profile == "full" then "nix-full" else "nix";
  includeNixDB = profile == "full";
  contents = [
    roomMcp playground serviceRunner tcpRelay
    (if backend == "claude" then pkgs.claude-code else pkgs.codex)
    pkgs.rustc pkgs.cargo pkgs.rustfmt pkgs.clippy
    pkgs.gitMinimal pkgs.stdenv.cc pkgs.pkg-config pkgs.ripgrep pkgs.fd
    pkgs.bash pkgs.coreutils pkgs.cacert
  ] ++ pkgs.lib.optionals (profile == "full") [ pkgs.nix pkgs.nodejs pkgs.jdk_headless pkgs.python3 pkgs.curl pkgs.path ];
  extraCommands = ''
    mkdir -p tmp home/agent workspace
    chmod 1777 tmp
    chmod 0777 home/agent workspace
  '' + pkgs.lib.optionalString (profile == "full") ''
    mkdir -p etc/nix
    cat > etc/nix/nix.conf <<'EOF'
    build-users-group =
    sandbox = false
    experimental-features = nix-command flakes
    accept-flake-config = false
    EOF
    # Existing store entries remain root-owned and immutable to the non-root worker.
    # New paths and the database live only in this container's private CoW layer.
    chmod 1777 nix/store
    mkdir -p nix/var/log/nix/drvs
    chmod -R a+rwX nix/var/nix nix/var/log/nix
    # Nix requires these two discovery directories to have mode 0755 and
    # otherwise attempts chmod, which the non-root worker cannot perform.
    mkdir -p nix/var/nix/profiles/per-user nix/var/nix/gcroots/per-user
    chmod 0755 nix/var/nix/profiles/per-user nix/var/nix/gcroots/per-user
  '';
  config = {
    Labels = { "dev.control.service-runner" = "1"; "dev.control.tcp-relay" = "2"; }
      // pkgs.lib.optionalAttrs (profile == "full") { "dev.control.worker-profile" = "full-v1"; };
    WorkingDir = "/workspace";
    Env = [
      "HOME=/home/agent"
      "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
      "PATH=/bin${pkgs.lib.optionalString (profile == "full") ":${pkgs.jdk_headless}/bin"}"
    ] ++ pkgs.lib.optionals (profile == "full") [
      "NIX_REMOTE=local" "NIX_PATH=nixpkgs=${pkgs.path}" "USER=agent"
      "JAVA_HOME=${pkgs.jdk_headless}"
      "XDG_STATE_HOME=/home/agent/.local/state"
      "XDG_CONFIG_HOME=/home/agent/.config"
      "XDG_CACHE_HOME=/home/agent/.cache"
    ];
    Cmd = [ (if backend == "claude" then "claude" else "codex") "--version" ];
  };
}
