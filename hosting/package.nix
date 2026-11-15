{ pkgs, withTerminal ? true, revision ? null }:
let
  inherit (pkgs) lib;
  # Keep the UI output reusable when only the host code changes.
  ui = pkgs.stdenv.mkDerivation {
    pname = "control-web";
    version = "0.1.0";
    src = lib.fileset.toSource {
      root = ../.;
      fileset = lib.fileset.unions [
        ../web/Cargo.toml ../web/Cargo.lock ../web/Trunk.toml
        ../web/index.html ../web/window.css ../web/src ../crates/context
      ];
    };
    cargoRoot = "web";
    cargoDeps = pkgs.rustPlatform.importCargoLock { lockFile = ../web/Cargo.lock; };
    nativeBuildInputs = [
      pkgs.cargo pkgs.rustc pkgs.lld pkgs.rustPlatform.cargoSetupHook
      pkgs.trunk pkgs.wasm-bindgen-cli_0_2_129
    ];
    buildPhase = ''
      runHook preBuild
      export HOME=$TMPDIR
      (cd web && trunk build --release --locked --offline --skip-version-check)
      runHook postBuild
    '';
    installPhase = ''
      mkdir -p $out
      cp -r web/dist/. $out/
    '';
  };
in
pkgs.rustPlatform.buildRustPackage {
  pname = "control";
  version = "0.1.0";
  src = lib.cleanSourceWith {
    src = ../.;
    filter = path: type:
      lib.cleanSourceFilter path type
      && !(builtins.elem (builtins.baseNameOf path) [ "target" "dist" ]);
  };
  CONTROL_BUILD_REVISION = if revision == null then "" else revision;
  cargoLock.lockFile = ../Cargo.lock;
  cargoBuildFlags = [ "--bin" "control" ] ++ lib.optional (!withTerminal) "--no-default-features";
  cargoTestFlags = [ "--workspace" ] ++ lib.optional (!withTerminal) "--no-default-features";
  nativeCheckInputs = [ pkgs.gitMinimal ] ++ lib.optional pkgs.stdenv.hostPlatform.isLinux pkgs.util-linux;
  preBuild = ''
    mkdir -p web/dist
    cp -r ${ui}/. web/dist/
  '';
  meta = {
    description = "Local swarm management platform with an embedded Rust/WASM UI";
    mainProgram = "control";
    platforms = lib.platforms.linux ++ lib.platforms.darwin;
  };
}
