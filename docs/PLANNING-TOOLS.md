# Planning tools

Two small, generic classical-AI tools let agents compute plans and routes
instead of reasoning them out in tokens. Both are pure Rust, read-only and
bounded; neither knows anything about a particular task.

## Room plan: `control_room_plan`

For whoever steers or assigns a team's work. It reads the room's
[claims](ROOM-NOTIFICATIONS.md#claims) and returns one compact view:

- `open`: working and blocked claims with owner, status and `blocked_on`;
  `done` counts finished claims.
- `unclaimed`: keys a blocked claim needs that nobody holds (never claimed, or
  released).
- `cycles`: circular waits through `blocked_on`.
- `critical_path`: the longest chain of unfinished dependencies, producer first.
  Chains that lead into a cycle never finish and are left out.
- `bottlenecks`: unfinished claims (or unclaimed keys) ranked by the members
  and claims that wait on them directly or transitively. This is the same walk
  as the [team advisor](LENSES.md#team-advisor)'s bottleneck fact
  (`rooms::claims::graph`), so the two never disagree; a test keeps them equal.
- `ready`: the parallel frontier, claims with no unfinished dependency:
  `working`, `resumable` (its dependency is done, so the owner can continue),
  `unclaimed` and `abandoned`. Critical-path work comes first, then whatever
  unblocks the most (`unblocks`), as in critical-path list scheduling.
- `available`: linked participants and claim owners holding no live working
  claim (idle, or only blocked), with `idle` when they have no active turn.

Each blocked claim names one dependency, so the graph is a functional graph and
every result is a linear walk over at most 128 claims per room. The tool uses
the room access rules of `control_room_read`: omit `room` for the team room;
another task's thread is refused. It assigns, releases and wakes nothing. Judges,
including the advisor, may call it.

## Path search: `control_search_path`

A* over a graph or grid supplied inline, or Dijkstra when no heuristic applies
(`control.search.path` on the operator MCP bridge). Use it for routing,
navigation, move or state-space search with an explicit graph.

**Graph mode.** `nodes` are ids (strings or integers) or
`{id, at:[x,y(,z)], h}`; nodes named only by edges are added. `edges` are
`[from, to]`, `[from, to, cost]` or `{from, to, cost}` (cost defaults to 1),
undirected unless `directed: true`. `heuristic` is `auto` (explicit `h`, else
coordinates, else none), `none`, `euclidean`, `manhattan` or `explicit`.
Coordinate estimates are scaled by the smallest edge cost/length ratio when an
edge is cheaper than its length (`heuristic_scale` in the result), so they stay
consistent and the path stays optimal. Explicit `h` values are checked:
`consistent: false` means nodes may be re-expanded; the result is optimal if
the values never overestimate.

**Grid mode.** `grid.size` is `[w, h]` or `[w, h, d]`; 2D grids can instead (or
also) give `rows` of text: `#` blocked, digits `1`–`9` cell cost, anything else
cost 1. `blocked` lists cells and `costs` gives a flat cost per cell, x fastest.
`moves` is 4 or 8 in 2D and 6 or 26 in 3D. Entering a cell costs its cost times
the step length (1, √2, √3); diagonal moves never cut a blocked corner. The
estimate is the Manhattan or octile distance times the cheapest cell cost.
Cells are `[x, y]` or `[x, y, z]`.

**Common fields.** `start`, `goal` or `goals` (the nearest reachable goal wins),
`avoid` (nodes or cells), `avoid_edges`, `max_cost` (inclusive) and
`max_expansions`. The result has `found`, `path`, `cost`, `steps`, `goal`,
`expanded`, `algorithm` and `heuristic`; an unreachable goal or one beyond
`max_cost` returns `found: false` with a `reason`.

Limits: 20,000 nodes, 100,000 edges, 1,000,000 grid cells, 256 goals,
2,000,000 expansions and 2 seconds of search; exceeding one is an error that
names it. Worker requests also pass through the 32 KiB tool mailbox, so large
graphs come from the operator bridge; compact `rows` suit spatial tasks.
[Timings](PERFORMANCE.md#tools).

## Not here

Routines (behaviour trees, state machines) belong to the workflow layer; see
[H1 in the roadmap](SWARM-ROADMAP.md#h1-layered-control--the-model-decides-routines-react).
