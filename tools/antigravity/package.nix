# Google's Antigravity CLI (`agy`), the unmodified release binary pinned by the
# version and SHA-512 its installer's manifest publishes (no licence is stated:
# treated as unfree), plus `control-agy`, the container entrypoint.
{ pkgs }:
let
  lib = pkgs.lib;
  version = "1.5.14";
  build = "5302690847260672";
  release = {
    aarch64-linux = { dir = "linux-arm"; file = "cli_linux_arm64.tar.gz"; hash = "sha512-ybKbukdfY6GoAWGuk7pysiHHpYbZhR5NAGFUZwS6tm1rJ2CyXXFtGRck/HVk8yRkVu4VHiiqN69I281yvcbEYw=="; };
    x86_64-linux = { dir = "linux-x64"; file = "cli_linux_x64.tar.gz"; hash = "sha512-gzEdPzVFZ2EtwWwa4uqbUD1vaDOqThONOaoinaPzu5mw5NfMoLzkeWsAYNalHYET63RJ7V3A55AL8hcQfqW/+Q=="; };
  }.${pkgs.stdenv.hostPlatform.system};
  agy = pkgs.stdenv.mkDerivation {
    pname = "antigravity-cli";
    inherit version;
    src = pkgs.fetchurl {
      url = "https://storage.googleapis.com/antigravity-public/antigravity-cli/${version}-${build}/${release.dir}/${release.file}";
      inherit (release) hash;
    };
    sourceRoot = ".";
    # A glibc Go binary: only its interpreter and libc search path change.
    nativeBuildInputs = [ pkgs.autoPatchelfHook ];
    dontConfigure = true;
    dontBuild = true;
    dontStrip = true;
    installPhase = ''
      install -Dm755 antigravity $out/bin/agy
    '';
    meta = {
      description = "Google Antigravity CLI";
      homepage = "https://antigravity.google/docs/cli/headless";
      license = lib.licenses.unfree;
      sourceProvenance = [ lib.sourceTypes.binaryNativeCode ];
      platforms = [ "aarch64-linux" "x86_64-linux" ];
      mainProgram = "agy";
    };
  };
  # control-agy turn|text|login|check [AGY FLAGS...]
  # Control's private files arrive read-only under /home/agent/.control; the
  # CLI's own state lives on a tmpfs and dies with the container. In a
  # container the CLI keeps a Google sign-in in this file, not the OS keyring.
  entrypoint = pkgs.writeShellApplication {
    name = "control-agy";
    runtimeInputs = [ agy pkgs.jq pkgs.coreutils pkgs.findutils ];
    text = ''
      mode=''${1:-}
      shift || true
      umask 077
      control=/home/agent/.control
      token=$HOME/.gemini/antigravity-cli/antigravity-oauth-token
      mkdir -p "$HOME/.gemini/antigravity-cli" "$HOME/.gemini/config"
      if [ -f "$control/settings.json" ]; then cp "$control/settings.json" "$HOME/.gemini/antigravity-cli/settings.json"; fi
      if [ -f "$control/mcp_config.json" ]; then cp "$control/mcp_config.json" "$HOME/.gemini/config/mcp_config.json"; fi
      if [ -f /home/agent/.control-key/gemini-api-key ]; then
        GEMINI_API_KEY=$(cat /home/agent/.control-key/gemini-api-key)
        export GEMINI_API_KEY
      fi
      case "$mode" in
        turn|text)
          # One prompt as one stream-json user message; the CLI exits after its result.
          if [ -f "$control/prompt" ]; then exec <"$control/prompt"; fi
          jq -Rsc '{event: "user", message: {content: .}}' | agy --input-format stream-json --output-format stream-json "$@"
          ;;
        login)
          agy "$@" || true
          if [ -s "$token" ]; then echo "sign-in saved: yes"; else echo "sign-in saved: no ($token was not written)"; fi
          # Names only, never contents.
          find "$HOME/.config/antigravity" -type f -printf 'also saved: .config/antigravity/%P\n' 2>/dev/null || true
          ;;
        check)
          if version=$(agy --version 2>&1 | head -n 1); then echo "binary: runs ($version)"; else echo "binary: did not run"; fi
          if [ -n "''${GEMINI_API_KEY:-}" ]; then echo "login: Gemini API key"
          elif [ -n "''${AGY_ADC_AUTH:-}" ]; then echo "login: Application Default Credentials"
          elif [ -s "$token" ]; then echo "login: Google sign-in token present"
          else echo "login: none mounted"; fi
          if agy models >/tmp/models 2>/tmp/models.err; then
            echo "models: $(grep -c . /tmp/models) listed"
            cut -c1-80 /tmp/models | head -n 40
          else
            echo "models: failed; last diagnostic line follows"
            tail -n 1 /tmp/models.err | cut -c1-300
            exit 1
          fi
          ;;
        *)
          echo "usage: control-agy turn|text|login|check [AGY FLAGS...]" >&2
          exit 2
          ;;
      esac
    '';
  };
in
pkgs.symlinkJoin {
  name = "antigravity-cli-${version}";
  paths = [ agy entrypoint ];
  meta = agy.meta;
}
