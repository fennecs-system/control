#!/usr/bin/env bash
# Cached `nix develop`: evaluates the flake's dev shell once and reuses it until
# flake.nix or flake.lock changes. The cache lives in .nix/ (gitignored); its
# profile link is a GC root, so `nix-collect-garbage` keeps the toolchain.
#
#   hosting/dev-env.sh                 interactive $SHELL in the dev environment
#   hosting/dev-env.sh -- CMD [ARG…]   run one command in it
#   hosting/dev-env.sh --print         print the env script path (for `source` or direnv)
#   CONTROL_DEV_REFRESH=1 …            rebuild the cache first
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
system="$(uname -m)-$(uname -s)"
key="$(cat "$root/flake.nix" "$root/flake.lock" <(printf '%s' "$system") | shasum -a 256 | cut -c1-16)"
cache="$root/.nix/dev-$key"

if [[ -n "${CONTROL_DEV_REFRESH:-}" || ! -s "$cache/env.sh" || ! -e "$cache/profile" ]]; then
  # Build in place: nix registers the profile as a GC root by its path.
  find "$root/.nix" -mindepth 1 -maxdepth 1 -name "dev-*" -exec rm -rf {} + 2>/dev/null || true
  mkdir -p "$cache"
  (cd "$root" && nix print-dev-env --profile "$cache/profile") > "$cache/env.sh.partial"
  mv "$cache/env.sh.partial" "$cache/env.sh"
fi

case "${1:-}" in
  --print)
    printf '%s\n' "$cache/env.sh"
    ;;
  --)
    shift
    [[ $# -gt 0 ]] || { echo "Usage: hosting/dev-env.sh -- CMD [ARG…]" >&2; exit 2; }
    exec bash -c 'source "$1"; shift; exec "$@"' _ "$cache/env.sh" "$@"
    ;;
  "")
    # Not exec'd from the env script, so its EXIT trap still removes its temp dir.
    exec bash -c 'source "$1"; "$2"' _ "$cache/env.sh" "${SHELL:-bash}"
    ;;
  *)
    echo "Usage: hosting/dev-env.sh [--print | -- CMD [ARG…]]" >&2
    exit 2
    ;;
esac
