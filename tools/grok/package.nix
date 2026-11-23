# xAI's Grok Build CLI (Apache-2.0), built from its published source at a pinned
# revision. Upstream ships no tagged releases; `version` is the binary crate's.
# Plus `control-grok`, the entrypoint an API-key account's turn runs.
{ pkgs }:
let
  grok = pkgs.rustPlatform.buildRustPackage {
    pname = "grok-build";
    version = "1.0.45";
    src = pkgs.fetchFromGitHub {
      owner = "xai-org";
      repo = "grok-build";
      rev = "2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8";
      hash = "sha256-oyB1NIBKljHDIqMhvwX8j401L79z8kYaOXP0rS+lymk=";
    };
    cargoHash = "sha256-Z8bdIuOnyFiQSeAQe5y4F0DvOmZSodDHJBPSiwN9u54=";
    # Upstream targets a Neoverse V2 CPU on aarch64 Linux, which other ARM hosts
    # (an Apple Silicon container VM among them) cannot execute.
    postPatch = ''
      substituteInPlace .cargo/config.toml --replace-fail '"-C", "target-cpu=neoverse-v2", ' ""
    '';
    cargoBuildFlags = [ "-p" "xai-grok-pager-bin" ];
    # About 2 GiB per compiler job, so a small builder is not killed for memory.
    # The largest crate alone needs more than 3 GiB: the stock macOS Linux
    # builder (3 GiB, no swap) cannot build this; give it 8 GiB or more.
    preBuild = ''
      jobs=$(( $(grep MemTotal /proc/meminfo | tr -dc 0-9) / 2097152 ))
      export NIX_BUILD_CORES=$(( jobs < 1 ? 1 : jobs < NIX_BUILD_CORES ? jobs : NIX_BUILD_CORES ))
    '';
    nativeBuildInputs = [ pkgs.pkg-config pkgs.protobuf pkgs.cmake pkgs.perl pkgs.gitMinimal ];
    buildInputs = [ pkgs.openssl ];
    PROTOC = "${pkgs.protobuf}/bin/protoc";
    CARGO_INCREMENTAL = "0";
    # Two build scripts embed a ripgrep they would otherwise download.
    GROK_TOOLS_BUNDLE_RG_PATH = "${pkgs.ripgrep}/bin/rg";
    GROK_SHELL_BUNDLE_RG_PATH = "${pkgs.ripgrep}/bin/rg";
    doCheck = false;
    # Official installs ship the `xai-grok-pager` artifact as `grok`.
    postInstall = ''
      mv $out/bin/xai-grok-pager $out/bin/grok
    '';
    meta = {
      description = "Grok Build, xAI's terminal coding agent";
      homepage = "https://github.com/xai-org/grok-build";
      license = pkgs.lib.licenses.asl20;
      mainProgram = "grok";
    };
  };
  # control-grok [GROK ARGS...]: Grok's documented API-key path is the
  # XAI_API_KEY variable (docs/user-guide/02-authentication.md at the pinned
  # revision). The key arrives as a read-only file, never as a variable of the
  # container; it is exported here for this process alone. Control's MCP server
  # is started without it (crates/grok).
  entrypoint = pkgs.writeShellApplication {
    name = "control-grok";
    runtimeInputs = [ grok pkgs.coreutils ];
    text = ''
      key=/home/agent/.control-key/xai-api-key
      if [ -f "$key" ]; then
        XAI_API_KEY=$(cat "$key")
        export XAI_API_KEY
      fi
      exec grok "$@"
    '';
  };
in
pkgs.symlinkJoin {
  name = "grok-build-${grok.version}";
  paths = [ grok entrypoint ];
  meta = grok.meta;
}
