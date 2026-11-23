# llama.cpp ahead of the pinned nixpkgs release, for models whose support landed
# after it (Inkling 2 Mini's looped layers arrived after v1.0.0). Drop this
# overlay once nixpkgs ships a release at or past `rev`.
final: prev:
let
  rev = "94f3af3d09e74a0fb35f703194318d3b4037714a";
  build = "12644";
  stale = flag: prev.lib.hasPrefix "-DLLAMA_BUILD_NUMBER" flag || prev.lib.hasPrefix "-DLLAMA_BUILD_COMMIT" flag;
in {
  llama-cpp = prev.llama-cpp.overrideAttrs (old: {
    version = "1.0.0-unstable-2026-11-22";
    src = prev.fetchFromGitHub {
      owner = "ggml-org";
      repo = "llama.cpp";
      inherit rev;
      hash = "sha256-s3nK1YWNAMxwXMwyYrFi8ehkaFqpRU7hRj+LWCiIMsI=";
    };
    # Since v1.0.0 the chat UI is out of tree and off by default; Control starts the
    # server with --no-webui, so nothing of the old npm build remains.
    npmDeps = null;
    preConfigure = "";
    nativeBuildInputs = builtins.filter (dep: dep != prev.npmHooks.npmConfigHook) old.nativeBuildInputs;
    # Converter and quantiser tests, not the server: two need numpy and torch, and
    # the quantiser's type snapshot fails on this revision upstream.
    disabledTests = (old.disabledTests or [ ]) ++ [
      "test-conversion-fp8"
      "test-kv-cache-scales"
      "test-quant-type-selection"
    ];
    cmakeFlags = builtins.filter (flag: !(stale flag)) old.cmakeFlags ++ [
      "-DLLAMA_BUILD_UI=OFF"
      "-DLLAMA_USE_PREBUILT_UI=OFF"
      "-DLLAMA_BUILD_NUMBER=${build}"
      "-DLLAMA_BUILD_COMMIT=${builtins.substring 0 7 rev}"
    ];
  });
}
