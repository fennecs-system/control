# control-comonad

A focus inside a history, and the way to everything around it. Pure and std-only.

- `Store` — the store comonad over any `Space` (slices, trees, spools):
  `extract`, `seek`, `peek`, `extend`; laws are tested.
- `Tree` — append-only arena with a zipper `Focus`: O(1) moves, root and depth,
  allocation-free ancestor and subtree walks, one-pass bottom-up `fold`.
- `lens::cut` — an unbounded timeline as O(log n) aligned blocks, verbatim at
  the focus: the basis for unbounded chat history, where each block's summary is
  computed once and reused.

Benchmark in the pinned build image (offline):

    docker run --rm --network none -v "$PWD":/src:ro -w /src -e CARGO_TARGET_DIR=/tmp/t \
      "$(cat ~/.config/control/build-image-id)" cargo run --release --offline -p control-comonad --example bench

Profile one case (`bench <name>`) with `perf` and `inferno` in a privileged
container from the worker image, building the crate standalone with
`CARGO_PROFILE_RELEASE_DEBUG=true`.
