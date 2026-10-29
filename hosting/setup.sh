# Executed by the pinned tools in `nix run .#setup`, from the checkout root.
with_claude=false
case "${1:-}" in
  "") ;;
  --claude) with_claude=true; shift ;;
  --help|-h)
    printf '%s\n' 'Usage: nix run .#setup [-- --claude]' \
      'Build the release UI/binary and Codex worker; optionally add Claude Code.' \
      'Requires a running Docker-compatible engine and a Linux Nix builder.' \
      'Preserves private configuration and state. Does not sign in or start services.'
    exit 0 ;;
  *) printf '%s\n' "Unknown setup option: $1" >&2; exit 2 ;;
esac
if [ "$#" -ne 0 ]; then
  printf '%s\n' 'Unexpected setup arguments; use --help.' >&2
  exit 2
fi
if [ ! -f Cargo.toml ] || [ ! -f web/Trunk.toml ] || [ ! -f flake.lock ]; then
  printf '%s\n' 'Run nix run .#setup from the Control checkout root.' >&2
  exit 1
fi
if ! command -v docker >/dev/null || ! docker info >/dev/null 2>&1; then
  printf '%s\n' 'Start a Docker-compatible Linux engine and make its docker command available.' >&2
  exit 1
fi

printf '%s\n' 'Building the embedded Rust/WASM UI and release binary…'
(cd web && env -u NO_COLOR trunk build --release --locked)
cargo build --release --locked --bin control
./target/release/control config init
config_dir=$(./target/release/control config path)

# Build every requested image before changing any configured image pin.
printf '%s\n' 'Building Nix worker images (macOS requires a configured Linux builder)…'
worker_image=$(nix build ".#packages.${CONTROL_SETUP_LINUX_SYSTEM}.worker-image" --no-link --print-out-paths)
if "$with_claude"; then
  claude_image=$(nix build ".#packages.${CONTROL_SETUP_LINUX_SYSTEM}.claude-image" --no-link --print-out-paths)
fi
docker load -i "$worker_image"
worker_id=$(docker image inspect control-worker:nix --format '{{.Id}}')
if "$with_claude"; then
  docker load -i "$claude_image"
  claude_id=$(docker image inspect control-claude:nix --format '{{.Id}}')
fi

verify_pin() {
  local digest=${1#sha256:}
  if [ "${1#sha256:}" = "$1" ] || [ "${#digest}" -ne 64 ] || [[ "$digest" = *[!0-9a-f]* ]]; then
    printf '%s\n' 'The engine did not return an immutable SHA-256 image ID; existing pins were preserved.' >&2
    exit 1
  fi
}
verify_pin "$worker_id"
if "$with_claude"; then verify_pin "$claude_id"; fi
check_pin_target() {
  if [ -L "$config_dir/$1" ]; then
    printf '%s\n' "Refusing a symlink image pin: $1" >&2
    exit 1
  fi
}
check_pin_target worker-image-id
if "$with_claude"; then check_pin_target claude-image-id; fi

umask 077
pin_temp=''
trap 'if [ -n "$pin_temp" ]; then rm -f -- "$pin_temp"; fi' EXIT
save_pin() {
  local name=$1 image_id=$2
  if [ -L "$config_dir/$name" ]; then
    printf '%s\n' "Refusing a symlink image pin: $name" >&2
    exit 1
  fi
  pin_temp=$(mktemp "$config_dir/.$name.XXXXXXXX")
  printf '%s\n' "$image_id" > "$pin_temp"
  mv -f -- "$pin_temp" "$config_dir/$name"
  pin_temp=''
}
save_pin worker-image-id "$worker_id"
if "$with_claude"; then save_pin claude-image-id "$claude_id"; fi

printf '%s\n' 'Setup complete. Checking prerequisites (no model call or credential contents read)…'
if ! ./target/release/control doctor; then
  printf '%s\n' 'Build and image setup succeeded; follow the doctor advice before starting work.'
fi
printf '%s\n' 'Next: sign in to your chosen provider, then run ./target/release/control serve.' \
  'Open another terminal and run ./target/release/control open.'
