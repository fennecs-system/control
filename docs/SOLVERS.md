# Solvers

`control_solve` hands an optimisation or constraint model to an exact solver and
returns its verdict, objective and assignment. Agents call it like any other tool:
workers see `control_solve`, the operator bridge (`control mcp`) and HTTP MCP see
`control.solve`. Read-only judges may call it too; it changes nothing.

| Solver | Formats | Good for |
| --- | --- | --- |
| `highs` | `lp` (CPLEX LP), `mps` | Linear and mixed-integer programs: allocation, blending, flows, transport, knapsack-like selection. |
| `z3` | `smt2` (SMT-LIB2) | Consistency checks, configuration and dependency constraints, bit-vector and arithmetic puzzles; `(minimize)`/`(maximize)` for small optimisation. |
| `cp-sat` | `mzn` (MiniZinc) | Scheduling, rostering, assignment, packing and routing with `alldifferent`, `cumulative`, `table` and similar global constraints. |

## When to use it

Use a solver when the answer is a search over many combinations, or when a claim
("these constraints are consistent", "this schedule is optimal") needs a proof
rather than a plausible guess. Typical cases:

- assignment (people to shifts, tasks to machines, reviewers to papers);
- scheduling with precedence, deadlines and shared resources;
- bin packing, cutting and knapsack selection;
- small routing and tour problems;
- checking that requirements, versions or invariants can all hold at once, or
  finding a counterexample when they cannot.

Write the model, call the tool, and report the solver's status with the result.
An `optimal` status is a proof for the model as written; `feasible` and `timeout`
results with `has_solution: true` are valid but possibly improvable assignments.

## Calling it

```json
{"solver": "cp-sat", "model": "...", "time_limit_seconds": 10}
```

| Field | Meaning |
| --- | --- |
| `solver` | A registry ID: `highs`, `z3` or `cp-sat`. |
| `model` | Model text, at most 256 KiB. |
| `format` | Optional; defaults to the solver's first format (`lp`, `smt2`, `mzn`). |
| `time_limit_seconds` | 1–20, default 10. Passed to the solver as its own limit. |
| `memory_mb` | 256–4096, default 1024. The container's hard memory limit. |
| `nonzero_only` | Omit variables whose value is 0 or `false` (useful for large 0/1 models). |

Worker tool requests travel through a 32 KiB mailbox envelope, so inline models
from workers are limited to roughly 30 KiB. For larger models or longer solves,
write the model into the task workspace and run the solver as a
[workspace job](COMMAND-JOBS.md) with `environment: "solver"`:

```json
{"command": "highs --model_file plan.lp --time_limit 600 --solution_file plan.sol", "environment": "solver", "timeout_seconds": 900}
```

That job uses the same image with the workspace mounted, the usual job limits
(1–1800 s, 2 GiB, 2 CPUs, no network) and the solvers' native output.

### Result

```json
{
  "solver": "cp-sat", "format": "mzn",
  "status": "optimal", "solver_status": "OPTIMAL_SOLUTION", "has_solution": true,
  "objective": 13,
  "variables": {"task": [2, 1, 3, 4]},
  "variable_count": 1, "variables_returned": 1, "variables_truncated": false,
  "messages": [],
  "elapsed_ms": 281, "exit_code": 0,
  "limits": {"time_limit_seconds": 5, "memory_mb": 1024, "cpus": 2},
  "provenance": {"image": "sha256:…", "package": "or-tools", "version": "9.15", "frontend": "minizinc 2.10.1"}
}
```

`status` is one of:

| Status | Meaning |
| --- | --- |
| `optimal` | Proven optimal (or, for Z3, optimal for the stated objectives). |
| `feasible` | A solution satisfying every constraint; no objective, or no optimality proof. |
| `infeasible` | Proven to have no solution (`unsat` for Z3). |
| `unbounded` | The objective can improve without limit. |
| `infeasible_or_unbounded` | The solver proved one of the two but not which. |
| `timeout` | The time limit ended the search. `has_solution` says whether the best solution found so far is included. |
| `unknown` | The solver stopped without a verdict (Z3 `unknown`, an iteration limit). |
| `error` | The model did not parse or the solver failed; see `messages` and `diagnostics`. |

`solver_status` keeps the solver's own wording. `variables` holds at most 2,000
entries and 128 KiB, in the solver's order; `variable_count` is the total and
`variables_truncated` marks the cut. Values are JSON numbers, booleans or arrays
where the solver reports them; other values (bit-vectors, functions, arrays in
SMT-LIB) are their solver text. Z3 reports several objectives in `objectives`.
`messages` collects solver errors and warnings with line numbers; `diagnostics`
holds the tail of the solver's log when the status is not a clean verdict.
`provenance` names the exact image and package versions that produced the result.

## Examples

### HiGHS: integer production plan

```text
Maximize
 profit: 3 x + 4 y
Subject To
 labour: x + 2 y <= 14
 ratio:  3 x - y >= 0
 spread: x - y <= 2
Bounds
 0 <= x <= 10
 0 <= y <= 10
General
 x y
End
```

`{"solver":"highs","model":"<above>"}` returns `optimal`, objective `34`,
`{"x":6,"y":4}`. Use `Binary` instead of `General` for 0/1 variables.

### Z3: consistency check

```smt2
(declare-const a Bool)
(declare-const b Bool)
(assert (xor a b))
(assert (=> a b))
(check-sat)
(get-model)
```

Returns `feasible` with `{"a":false,"b":true}`. Add `(maximize ...)` or
`(minimize ...)` with `(get-objectives)` for an objective; an `unsat` result is
`infeasible`, which proves the assertions contradict each other.

### CP-SAT through MiniZinc: assignment

```minizinc
include "alldifferent.mzn";
int: n = 4;
array[1..n, 1..n] of int: cost = [|9,2,7,8|6,4,3,7|5,8,1,8|7,6,9,4|];
array[1..n] of var 1..n: task;
constraint alldifferent(task);
solve minimize sum(w in 1..n)(cost[w, task[w]]);
```

Returns `optimal`, objective `13`, `{"task":[2,1,3,4]}` (worker `w` takes task
`task[w]`). MiniZinc data belongs in the model; separate `.dzn` files are not
accepted. Every top-level decision variable is reported unless the model has an
output item.

## Isolation and limits

Each call runs one fresh container from the operator-pinned solver image:

- no network (`--network=none`), no mounts, no credentials and no host files;
- read-only root, all capabilities dropped, `no-new-privileges`, user 65534;
- memory and swap capped at `memory_mb`, 2 CPUs, 128 processes, and a
  RAM-backed `/tmp` of half the memory limit (at most 512 MiB) for the model and
  compiled intermediates;
- the model arrives on standard input; output is read through bounded pipes
  (1 MiB of results, the last 64 KiB of log);
- Control stops the container at the time limit plus 8 s of start-up grace, so a
  solver that ignores its own limit still ends as `timeout`;
- at most two solver calls run at once per node; a further call returns
  "Solver tool is busy" instead of queueing.

The container image never changes during a call, and nothing from a run persists.

## Installing the image

Build and pin the image from a Control checkout:

```sh
nix run .#setup -- --solver
```

On macOS this needs the [Linux builder](setup.md#linux-builder-on-macos). Setup
builds `solver-image` (`tools/solver/image.nix`) from the pinned Nixpkgs, loads
it and records its immutable ID in `solver-image-id` in the node configuration
directory. `CONTROL_SOLVER_IMAGE` overrides that pin with an image ID or digest.
`control doctor` reports the pinned image and the solvers it provides. Without a
pin, the tool returns an error naming the setup command.

The image contains the solver command-line programs, Bash and coreutils. It keeps
only the CP-SAT FlatZinc executable from or-tools (not its Python bindings) and
builds MiniZinc without Gecode's Qt visualiser, so neither Python nor a GUI stack
enters the image. The first build compiles MiniZinc; the rest comes from the
binary cache.

## Adding a solver

A solver is one Nix package and one registry entry:

1. Add the package to `solvers` in `tools/solver/image.nix`. Its `pname` and
   `version` become the image's `dev.control.solvers` label, which Control
   reads for provenance and to check that the pinned image provides the solver.
2. Add a `Solver` to `SOLVERS` in `src/solver.rs`: an ID, a summary, its formats,
   the command line (`Invocation` carries the model path, time limit, memory and
   thread count) and a parser. Reuse a parser when the output format is shared:
   another MiniZinc backend (Gecode, Chuffed) needs only its `.msc` on
   `MZN_SOLVER_PATH` and `command: |run| minizinc_command("<solver tag>", run)`.
   A solver that only writes a result file sets `result_file`; its log then goes
   to the diagnostics and the file becomes the parsed output.
3. Add fixture outputs to `src/solver/tests.rs` for its statuses, extend the
   ignored live test with a tiny model, and rebuild the image.

The tool schema, its solver list and the description are generated from the
registry. New formats also need adding to the schema's `format` list.

## Verification

```sh
cargo test --locked --bin control solver
CONTROL_SOLVER_IMAGE=$(cat ~/.config/control/solver-image-id) \
  cargo test --locked --bin control solver::tests::live -- --ignored
```

The unit tests cover registry and schema agreement, input bounds, container
arguments, and status/assignment parsing from recorded HiGHS, Z3 and MiniZinc
output. The ignored live test solves the same small model with every solver in
its container and checks infeasible, syntax-error and timeout results.
