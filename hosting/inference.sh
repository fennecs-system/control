# Executed by the pinned tools in `nix run .#inference`, from the checkout root.
# Starts the node's local llama.cpp service with the backend this platform needs:
# Metal as a host launchd agent on Apple Silicon (Docker on macOS has no GPU),
# otherwise the Nix-built Linux container (CUDA when the NVIDIA runtime is present,
# else CPU). The binary always comes from the pinned nixpkgs; weights are never
# downloaded. The server binds loopback only.
model='' alias=qwen-local ctx=32768 port=8081 parallel=1 sha='' action=start dry=false
backend=${CONTROL_INFERENCE_BACKEND:-} acceleration=''
extra=()
usage() {
  printf '%s\n' 'Usage: nix run .#inference -- --model /abs/model.gguf [--backend auto|metal|container|cpu]' \
    '         [--acceleration cpu|vulkan|cuda] [--alias NAME] [--ctx-size N] [--port N] [--parallel N]' \
    '         [--sha256 HEX] [--dry-run] [-- extra llama-server flags]' \
    '       nix run .#inference -- --status | --stop' \
    'auto selects metal on aarch64-darwin, otherwise the container (cuda if the NVIDIA runtime is' \
    'available, else cpu). metal runs the pinned llama-server as the launchd agent dev.control.inference;' \
    'container/cpu run the pinned image as container control-inference. The choice is saved in' \
    'inference.json in the Control configuration directory and reused when --backend is omitted.'
}
while [ "$#" -ne 0 ]; do
  case "$1" in
    --model) model=$2; shift ;;
    --backend) backend=$2; shift ;;
    --acceleration) acceleration=$2; shift ;;
    --alias) alias=$2; shift ;;
    --ctx-size) ctx=$2; shift ;;
    --port) port=$2; shift ;;
    --parallel) parallel=$2; shift ;;
    --sha256) sha=$2; shift ;;
    --status) action=status ;;
    --dry-run) dry=true ;;
    --stop) action=stop ;;
    --help|-h) usage; exit 0 ;;
    --) shift; extra=("$@"); break ;;
    *) printf '%s\n' "Unknown option: $1; use --help." >&2; exit 2 ;;
  esac
  shift
done

config_dir=${CONTROL_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/control}
data_dir="${XDG_DATA_HOME:-$HOME/.local/share}/control/inference"
mkdir -p "$config_dir" "$data_dir"
chmod 700 "$config_dir" "$data_dir"
label=dev.control.inference
plist="$HOME/Library/LaunchAgents/$label.plist"
os=$(uname -s) arch=$(uname -m)

saved() {
  if [ -f "$config_dir/inference.json" ]; then
    sed -n "s/.*\"$1\": *\"\{0,1\}\([^\",}]*\).*/\1/p" "$config_dir/inference.json" | head -n 1
  fi
}
if [ -z "$backend" ]; then backend=$(saved backend); fi
if [ -z "$backend" ] || [ "$backend" = auto ]; then
  if [ "$os" = Darwin ] && [ "$arch" = arm64 ]; then backend=metal; else backend=container; fi
fi
case "$backend" in metal|container|cpu) ;; *) printf '%s\n' "Unknown backend: $backend (metal|container|cpu)" >&2; exit 2 ;; esac
if [ "$backend" = metal ] && { [ "$os" != Darwin ] || [ -z "${CONTROL_LLAMA_SERVER:-}" ]; }; then
  printf '%s\n' 'The metal backend needs Apple Silicon macOS; use --backend container.' >&2
  exit 2
fi

stop_all() {
  if [ "$os" = Darwin ]; then launchctl bootout "gui/$(id -u)/$label" 2>/dev/null || true; fi
  if command -v docker >/dev/null && docker info >/dev/null 2>&1; then docker rm -f control-inference >/dev/null 2>&1 || true; fi
}
health() { curl -fsS --max-time 2 "http://127.0.0.1:$port/health" 2>/dev/null; }
case "$action" in
  stop)
    stop_all
    if [ -f "$plist" ]; then rm -f -- "$plist"; fi
    printf '%s\n' 'Stopped the local inference service (configuration kept).'
    exit 0 ;;
  status)
    if [ -f "$config_dir/inference.json" ]; then cat "$config_dir/inference.json"; fi
    if health >/dev/null; then printf '%s\n' "Serving on http://127.0.0.1:$port/v1"; else printf '%s\n' "Not answering on port $port"; exit 1; fi
    exit 0 ;;
esac

case "$model" in /*) ;; *) printf '%s\n' 'Pass --model with an absolute path to a GGUF file outside the checkout.' >&2; exit 2 ;; esac
if [ ! -f "$model" ] || [ -L "$model" ]; then printf '%s\n' "Model is not a regular file: $model" >&2; exit 2; fi
for value in "$ctx" "$port" "$parallel"; do
  case "$value" in ''|*[!0-9]*) printf '%s\n' 'ctx-size, port and parallel must be positive integers.' >&2; exit 2 ;; esac
done
case "$alias" in ''|*[!A-Za-z0-9._:-]*) printf '%s\n' 'Alias must use letters, digits, dots, colons, dashes or underscores.' >&2; exit 2 ;; esac
if [ -z "$sha" ]; then
  size=$(wc -c < "$model" | tr -d ' ')
  if [ "$(saved model)" = "$model" ] && [ "$(saved bytes)" = "$size" ]; then sha=$(saved sha256); fi
  if [ -z "$sha" ]; then
    printf '%s\n' "Hashing $model for provenance (once)…"
    sha=$(sha256sum "$model" | cut -d' ' -f1)
  fi
fi
case "$sha" in *[!0-9a-f]*|'') printf '%s\n' 'Invalid SHA-256.' >&2; exit 2 ;; esac
[ "${#sha}" -eq 64 ] || { printf '%s\n' 'Invalid SHA-256.' >&2; exit 2; }

# Shared flags: one alias, no web UI, OpenAI tool calls through the model's
# chat template, and no hidden reasoning spending the output allowance.
server_flags=(--no-webui --alias "$alias" --ctx-size "$ctx" -np "$parallel" --jinja --reasoning-budget 0)
# --dry-run prints the launchd agent or container command and changes nothing.
if [ "$dry" = false ]; then stop_all; fi
if [ "$backend" = metal ]; then
  runtime="llama.cpp $("$CONTROL_LLAMA_SERVER/bin/llama-server" --version 2>&1 | sed -n 's/^version: \(.*\)$/\1/p') Metal"
  xml() { printf '%s' "$1" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g'; }
  args=("$CONTROL_LLAMA_SERVER/bin/llama-server" --host 127.0.0.1 --port "$port" --model "$model" "${server_flags[@]}" -ngl 999 "${extra[@]}")
  render() {
    printf '%s\n' '<?xml version="1.0" encoding="UTF-8"?>' \
      '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">' \
      '<plist version="1.0"><dict>' "  <key>Label</key><string>$label</string>" '  <key>ProgramArguments</key><array>'
    for arg in "${args[@]}"; do printf '    <string>%s</string>\n' "$(xml "$arg")"; done
    printf '%s\n' '  </array>' "  <key>WorkingDirectory</key><string>$(xml "$data_dir")</string>" \
      '  <key>RunAtLoad</key><true/>' '  <key>KeepAlive</key><true/>' '  <key>ThrottleInterval</key><integer>10</integer>' \
      '  <key>ProcessType</key><string>Interactive</string>' \
      "  <key>StandardOutPath</key><string>$(xml "$data_dir/server.log")</string>" \
      "  <key>StandardErrorPath</key><string>$(xml "$data_dir/server.log")</string>" '</dict></plist>'
  }
  if [ "$dry" = true ]; then
    printf '%s\n' "launchd agent $plist:"
    render
    exit 0
  fi
  mkdir -p "$HOME/Library/LaunchAgents"
  render > "$plist.tmp"
  mv -f -- "$plist.tmp" "$plist"
  launchctl bootstrap "gui/$(id -u)" "$plist"
else
  if ! command -v docker >/dev/null || ! docker info >/dev/null 2>&1; then
    printf '%s\n' 'Start a Docker-compatible engine for the container backend.' >&2; exit 1
  fi
  if [ "$backend" = cpu ]; then acceleration=cpu; fi
  if [ -z "$acceleration" ]; then
    acceleration=cpu
    if [ "$os" = Linux ] && command -v nvidia-smi >/dev/null && nvidia-smi -L >/dev/null 2>&1 \
      && docker info --format '{{json .Runtimes}}' | grep -q nvidia; then acceleration=cuda; fi
  fi
  case "$acceleration" in cpu) output=inference-image tag=nix gpu=() ;;
    vulkan) output=inference-image-vulkan tag=nix-vulkan gpu=(--device /dev/dri) ;;
    cuda) output=inference-image-cuda tag=nix-cuda gpu=(--gpus all) ;;
    *) printf '%s\n' "Unknown acceleration: $acceleration" >&2; exit 2 ;; esac
  runtime="llama.cpp container control-inference:$tag ($acceleration)"
  memory=${CONTROL_INFERENCE_MEMORY:-32g}
  run=(docker run -d --name control-inference --restart unless-stopped --read-only
    --cap-drop ALL --security-opt no-new-privileges --tmpfs "/tmp:rw,noexec,nosuid,size=64m"
    --memory "$memory" --memory-swap "$memory" "${gpu[@]}"
    -p "127.0.0.1:$port:8081"
    --mount "type=bind,src=$(dirname "$model"),dst=/models,readonly"
    "control-inference:$tag" --host 0.0.0.0 --port 8081 --model "/models/$(basename "$model")"
    "${server_flags[@]}" "${extra[@]}")
  if [ "$dry" = true ]; then
    printf '%s\n' "nix build .#packages.${CONTROL_INFERENCE_LINUX_SYSTEM}.${output}; docker load; then:"
    printf '%q ' "${run[@]}"; printf '\n'
    exit 0
  fi
  image_path=$(nix build ".#packages.${CONTROL_INFERENCE_LINUX_SYSTEM}.${output}" --out-link "$data_dir/$output" --print-out-paths)
  docker load -i "$image_path" >/dev/null
  "${run[@]}" >/dev/null
  backend=container
fi

revision="$(basename "$model") sha256:$sha; $runtime; ctx $ctx"
umask 077
printf '{\n  "backend": "%s",\n  "acceleration": "%s",\n  "model": "%s",\n  "bytes": "%s",\n  "sha256": "%s",\n  "alias": "%s",\n  "ctx_size": %s,\n  "port": %s,\n  "url": "http://127.0.0.1:%s/v1",\n  "revision": "%s"\n}\n' \
  "$backend" "${acceleration:-metal}" "$model" "$(wc -c < "$model" | tr -d ' ')" "$sha" "$alias" "$ctx" "$port" "$port" "$revision" \
  > "$config_dir/inference.json"
printf '%s\n' "Waiting for the model to load on port $port…"
for _ in $(seq 1 180); do
  if health >/dev/null; then
    printf '%s\n' "Serving $alias on http://127.0.0.1:$port/v1 ($backend)." \
      'Add it once in Manage → Models & providers → + New summary model:' \
      "  Adapter: Local endpoint · Name: $alias · Model ID: $alias · Endpoint: http://127.0.0.1:$port/v1" \
      "  Revision: $revision" \
      "  Context tokens: $ctx · tick \"Serves tool calls\"" \
      "Then select it in a chat with /model local:$alias."
    exit 0
  fi
  sleep 2
done
printf '%s\n' "The server did not become healthy; see $data_dir/server.log or docker logs control-inference." >&2
exit 1
