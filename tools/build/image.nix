# An operator builds this declaration, then pins its immutable image ID. Agents
# select that environment; they never evaluate repository Nix on the host.
{ pkgs
, cargoLocks ? [ { lockFile = ../../Cargo.lock; } { lockFile = ../../web/Cargo.lock; } ]
, extraPackages ? [ ]
, wasmBindgen ? pkgs.wasm-bindgen-cli_0_2_129
}:
let
  vendors = map (lock: pkgs.rustPlatform.importCargoLock lock) cargoLocks;
  # Sharing immutable crate sources is safe; execution never publishes caches.
  # A duplicate name must point at the very same checksum-pinned source.
  vendor = pkgs.runCommand "control-build-vendor" { } ''
    mkdir -p "$out/vendor"
    for source in ${pkgs.lib.concatStringsSep " " vendors}; do
      for crate in "$source"/*; do
        name=$(basename "$crate")
        if [ "$name" = Cargo.lock ]; then continue; fi
        target=$(readlink "$crate")
        if [ -e "$out/vendor/$name" ]; then
          test "$(readlink "$out/vendor/$name")" = "$target"
        else
          ln -s "$target" "$out/vendor/$name"
        fi
      done
    done
  '';
  # Cargo subprocesses (notably cargo-clippy) also read this configuration.
  # A CLI-only wrapper loses source replacement when a tool launches Cargo.
  cargoConfig = pkgs.writeTextDir "opt/control-cargo/config.toml" ''
    [net]
    offline = true
    [source.crates-io]
    replace-with = "control-vendored"
    [source.control-vendored]
    directory = "${vendor}/vendor"
  '';
  toolchain = pkgs.buildEnv {
    name = "control-build-toolchain";
    paths = [
      pkgs.cargo pkgs.rustc pkgs.rustfmt pkgs.clippy pkgs.lld pkgs.trunk wasmBindgen
      pkgs.gitMinimal pkgs.stdenv.cc pkgs.pkg-config pkgs.ripgrep pkgs.fd
      pkgs.bash pkgs.coreutils pkgs.cacert
    ] ++ extraPackages;
    pathsToLink = [ "/bin" ];
  };
  provenance = pkgs.writeTextDir "etc/control-environment.json" (builtins.toJSON {
    schema = 1;
    kind = "rust-wasm-build";
    rust = pkgs.rustc.version;
    trunk = pkgs.trunk.version;
    wasmBindgen = wasmBindgen.version;
    locks = map (lock: builtins.hashFile "sha256" lock.lockFile) cargoLocks;
    offline = true;
    immutableBase = true;
  });
in
assert pkgs.lib.all (lock:
  pkgs.lib.all (package: !(package ? source)
    || package.source == "registry+https://github.com/rust-lang/crates.io-index")
    (builtins.fromTOML (builtins.readFile lock.lockFile)).package
) cargoLocks;
pkgs.dockerTools.buildLayeredImage {
  name = "control-build";
  tag = "nix";
  contents = [ toolchain cargoConfig provenance ];
  extraCommands = ''
    mkdir -p tmp home/agent workspace
    chmod 1777 tmp
    chmod 0777 home/agent workspace
  '';
  config = {
    WorkingDir = "/workspace";
    Env = [
      "HOME=/home/agent"
      "CARGO_HOME=/opt/control-cargo"
      "CARGO_NET_OFFLINE=true"
      "CARGO_BUILD_JOBS=2"
      "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
      "PATH=/bin"
    ];
    Labels."dev.control.environment" = "rust-wasm-build-v1";
    Cmd = [ "cargo" "--version" ];
  };
}
