# Executed by the pinned tools in `nix run .#inference`, from the checkout root.
# Starts one local llama.cpp service per model (keyed by --alias), with the
# backend this platform needs:
# Metal as a host launchd agent on Apple Silicon (Docker on macOS has no GPU),
# otherwise the Nix-built Linux container (CUDA when the NVIDIA runtime is present,
# else CPU). The binary always comes from the pinned nixpkgs; weights are never
# downloaded. The server binds loopback only.
model='' alias=qwen-local ctx=32768 port=8081 parallel=1 sha='' action=start dry=false alias_set=false
backend=${CONTROL_INFERENCE_BACKEND:-} acceleration=''
extra=()
usage() {
  printf '%s\n' 'Usage: nix run .#inference -- --model /abs/model.gguf [--backend auto|metal|container|cpu]' \
    '         [--acceleration cpu|vulkan|cuda] [--alias NAME] [--ctx-size N] [--port N] [--parallel N]' \
    '         [--sha256 HEX] [--dry-run] [-- extra llama-server flags]' \
    '       nix run .#inference -- --status | --stop [--alias NAME]' \
    'Each --alias is its own service: run it again with another --model, --alias and --port to serve' \
    'a second model. auto selects metal on aarch64-darwin, otherwise the container (cuda if the NVIDIA' \
    'runtime is available, else cpu). metal runs the pinned llama-server as the launchd agent' \
    'dev.control.inference.NAME; container/cpu run the pinned image as container control-inference-NAME.' \
    'Each model'\''s record is inference/NAME.json in the Control configuration directory; its backend' \
    'is reused when --backend is omitted. --status and --stop without --alias cover every model.'
}
while [ "$#" -ne 0 ]; do
  case "$1" in
    --model) model=$2; shift ;;
    --backend) backend=$2; shift ;;
    --acceleration) acceleration=$2; shift ;;
    --alias) alias=$2; alias_set=true; shift ;;
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
records="$config_dir/inference"
mkdir -p "$config_dir" "$data_dir" "$records"
chmod 700 "$config_dir" "$data_dir" "$records"
os=$(uname -s) arch=$(uname -m)
case "$alias" in ''|*[!A-Za-z0-9._:-]*) printf '%s\n' 'Alias must use letters, digits, dots, colons, dashes or underscores.' >&2; exit 2 ;; esac

# One service per model, keyed by its alias: launchd agent dev.control.inference.NAME
# or container control-inference-NAME, record inference/NAME.json, log NAME.log.
# Earlier releases ran a single unsuffixed service with record inference.json.
slug() { printf '%s' "${1//:/-}"; }
label_of() { printf 'dev.control.inference.%s' "$(slug "$1")"; }
container_of() { printf 'control-inference-%s' "$(slug "$1")"; }
record_of() { printf '%s/%s.json' "$records" "$(slug "$1")"; }
legacy="$config_dir/inference.json"
label=$(label_of "$alias") container=$(container_of "$alias") record=$(record_of "$alias")
plist="$HOME/Library/LaunchAgents/$label.plist"
log="$data_dir/$(slug "$alias").log"

field() {
  if [ -f "$2" ]; then
    sed -n "s/.*\"$1\": *\"\{0,1\}\([^\",}]*\).*/\1/p" "$2" | head -n 1
  fi
}
# This model's record, else the legacy record when it described this alias.
saved() {
  if [ -f "$record" ]; then field "$1" "$record"
  elif [ "$(field alias "$legacy")" = "$alias" ]; then field "$1" "$legacy"; fi
}
if [ -z "$backend" ]; then backend=$(saved backend); fi
if [ -z "$backend" ]; then backend=$(field backend "$legacy"); fi
if [ -z "$backend" ] || [ "$backend" = auto ]; then
  if [ "$os" = Darwin ] && [ "$arch" = arm64 ]; then backend=metal; else backend=container; fi
fi
case "$backend" in metal|container|cpu) ;; *) printf '%s\n' "Unknown backend: $backend (metal|container|cpu)" >&2; exit 2 ;; esac
if [ "$backend" = metal ] && { [ "$os" != Darwin ] || [ -z "${CONTROL_LLAMA_SERVER:-}" ]; }; then
  printf '%s\n' 'The metal backend needs Apple Silicon macOS; use --backend container.' >&2
  exit 2
fi

docker_up() { command -v docker >/dev/null && docker info >/dev/null 2>&1; }
# Stops one launchd agent/container pair and removes its agent file.
stop_service() {
  if [ "$os" = Darwin ]; then launchctl bootout "gui/$(id -u)/$1" 2>/dev/null || true; fi
  rm -f -- "$HOME/Library/LaunchAgents/$1.plist"
  if docker_up; then docker rm -f "$2" >/dev/null 2>&1 || true; fi
}
stop_legacy() { stop_service dev.control.inference control-inference; rm -f -- "$legacy"; }
health() { curl -fsS --max-time 2 "http://127.0.0.1:$1/health" >/dev/null 2>&1; }
report() {
  if health "$2"; then printf '%s\n' "$1: serving on http://127.0.0.1:$2/v1"; else printf '%s\n' "$1: not answering on port $2"; return 1; fi
}
case "$action" in
  stop)
    if [ "$alias_set" = true ]; then
      stop_service "$label" "$container"; rm -f -- "$record"
      if [ "$(field alias "$legacy")" = "$alias" ]; then stop_legacy; fi
      printf '%s\n' "Stopped $alias."
    else
      for file in "$records"/*.json; do
        [ -f "$file" ] || continue
        name=$(field alias "$file"); stop_service "$(label_of "$name")" "$(container_of "$name")"; rm -f -- "$file"
      done
      stop_legacy
      printf '%s\n' 'Stopped every local inference service.'
    fi
    exit 0 ;;
  status)
    status=0
    if [ "$alias_set" = true ]; then
      if [ -f "$record" ]; then cat "$record"; report "$alias" "$(field port "$record")" || status=1
      else printf '%s\n' "No record for $alias."; status=1; fi
    else
      found=false
      for file in "$records"/*.json "$legacy"; do
        [ -f "$file" ] || continue
        found=true; report "$(field alias "$file")" "$(field port "$file")" || status=1
      done
      if [ "$found" = false ]; then printf '%s\n' 'No local inference services configured.'; status=1; fi
    fi
    exit "$status" ;;
esac

case "$model" in /*) ;; *) printf '%s\n' 'Pass --model with an absolute path to a GGUF file outside the checkout.' >&2; exit 2 ;; esac
if [ ! -f "$model" ] || [ -L "$model" ]; then printf '%s\n' "Model is not a regular file: $model" >&2; exit 2; fi
for value in "$ctx" "$port" "$parallel"; do
  case "$value" in ''|*[!0-9]*) printf '%s\n' 'ctx-size, port and parallel must be positive integers.' >&2; exit 2 ;; esac
done
# Another model keeps its port; pick a free one for each additional model.
for file in "$records"/*.json; do
  if [ -f "$file" ] && [ "$file" != "$record" ] && [ "$(field port "$file")" = "$port" ]; then
    printf '%s\n' "Port $port already serves $(field alias "$file"); pass another --port for $alias, or stop that model first." >&2; exit 2
  fi
done
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
if [ "$dry" = false ]; then
  stop_service "$label" "$container"
  # The single-service layout yields this alias or port to the per-model service.
  if [ -f "$legacy" ] && { [ "$(field alias "$legacy")" = "$alias" ] || [ "$(field port "$legacy")" = "$port" ]; }; then stop_legacy; fi
fi
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
      "  <key>StandardOutPath</key><string>$(xml "$log")</string>" \
      "  <key>StandardErrorPath</key><string>$(xml "$log")</string>" '</dict></plist>'
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
  run=(docker run -d --name "$container" --restart unless-stopped --read-only
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
  > "$record"
printf '%s\n' "Waiting for the model to load on port $port…"
for _ in $(seq 1 180); do
  if health "$port"; then
    printf '%s\n' "Serving $alias on http://127.0.0.1:$port/v1 ($backend)." \
      'Add it once in Manage → Models & providers → + New text model:' \
      "  Adapter: Local endpoint · Name: $alias · Model ID: $alias · Endpoint: http://127.0.0.1:$port/v1" \
      "  Revision: $revision" \
      "  Context tokens: $ctx · tick \"Serves tool calls\" if this model runs agent turns" \
      "Then use it for agent turns with /model local:$alias, or as the compactor under History compaction."
    exit 0
  fi
  sleep 2
done
printf '%s\n' "The server did not become healthy; see $log or docker logs $container." >&2
exit 1
