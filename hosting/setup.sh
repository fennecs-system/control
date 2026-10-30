# Executed by the pinned tools in `nix run .#setup`, from the checkout root.
images=(worker)
provider=codex
while [ "$#" -ne 0 ]; do
  case "$1" in
    --claude) images+=(claude); provider=claude ;;
    --browser) images+=(browser) ;;
    --help|-h)
      printf '%s\n' 'Usage: nix run .#setup [-- --claude] [--browser]' \
        'Build the release UI/binary and Codex worker; optionally add Claude Code and the browser-capture image.' \
        'Requires a running Docker-compatible engine and a Linux Nix builder.' \
        'Preserves private configuration and state. Does not sign in or start services.'
      exit 0 ;;
    *) printf '%s\n' "Unknown setup option: $1; use --help." >&2; exit 2 ;;
  esac
  shift
done
if [ ! -f Cargo.toml ] || [ ! -f web/Trunk.toml ] || [ ! -f flake.lock ]; then
  printf '%s\n' 'Run nix run .#setup from the Control checkout root.' >&2
  exit 1
fi
if ! command -v docker >/dev/null || ! docker info >/dev/null 2>&1; then
  printf '%s\n' 'Start a Docker-compatible Linux engine and make its docker command available.' >&2
  exit 1
fi

# Build every requested image first: a missing Linux builder fails before the long UI build,
# and no configured image pin changes until all of them build and load.
printf '%s\n' "Building Nix ${CONTROL_SETUP_LINUX_SYSTEM} images: ${images[*]}…"
declare -A image_path image_id
for image in "${images[@]}"; do
  if ! image_path[$image]=$(nix build ".#packages.${CONTROL_SETUP_LINUX_SYSTEM}.${image}-image" --no-link --print-out-paths); then
    printf '%s\n' "Could not build the ${CONTROL_SETUP_LINUX_SYSTEM} ${image} image. On macOS this needs a Linux Nix builder;" \
      'see "Linux builder on macOS" in docs/setup.md.' >&2
    exit 1
  fi
done

printf '%s\n' 'Building the embedded Rust/WASM UI and release binary…'
(cd web && env -u NO_COLOR trunk build --release --locked)
env -u CARGO_TARGET_DIR cargo build --release --locked --bin control
./target/release/control config init
config_dir=$(./target/release/control config path)

verify_pin() {
  local digest=${1#sha256:}
  if [ "${1#sha256:}" = "$1" ] || [ "${#digest}" -ne 64 ] || [[ "$digest" = *[!0-9a-f]* ]]; then
    printf '%s\n' 'The engine did not return an immutable SHA-256 image ID; existing pins were preserved.' >&2
    exit 1
  fi
}
check_pin_target() {
  if [ -L "$config_dir/$1" ]; then
    printf '%s\n' "Refusing a symlink image pin: $1" >&2
    exit 1
  fi
}
for image in "${images[@]}"; do
  docker load -i "${image_path[$image]}"
  image_id[$image]=$(docker image inspect "control-${image}:nix" --format '{{.Id}}')
  verify_pin "${image_id[$image]}"
  check_pin_target "${image}-image-id"
done

umask 077
pin_temp=''
trap 'if [ -n "$pin_temp" ]; then rm -f -- "$pin_temp"; fi' EXIT
save_pin() {
  local name=$1 id=$2
  check_pin_target "$name"
  pin_temp=$(mktemp "$config_dir/.$name.XXXXXXXX")
  printf '%s\n' "$id" > "$pin_temp"
  mv -f -- "$pin_temp" "$config_dir/$name"
  pin_temp=''
}
for image in "${images[@]}"; do save_pin "${image}-image-id" "${image_id[$image]}"; done
./target/release/control config init --provider "$provider" --source-root "$(pwd -P)"

printf '%s\n' 'Setup complete. Checking prerequisites (no model call or credential contents read)…' \
  'Before you sign in, a provider credential failure below is expected.'
if ! ./target/release/control doctor; then
  printf '%s\n' 'Build and image setup succeeded; sign in, then follow any remaining doctor advice.'
fi
printf '%s\n' 'Next: sign in to your chosen provider, then run ./target/release/control serve.' \
  'Open another terminal and run ./target/release/control open.'
