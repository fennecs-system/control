//! Optimisation and constraint solvers as one host-brokered tool. Each call
//! runs a registered solver CLI in a fresh, offline, resource-bounded container
//! from the operator-pinned Nix solver image. The model arrives on stdin; the
//! solver's standard output is parsed into one status vocabulary.
//!
//! Adding a solver is one Nix package in `tools/solver/image.nix` and one
//! [`Solver`] entry in [`SOLVERS`]: its formats, command line and output parser
//! (reuse an existing parser for a format it shares). See docs/SOLVERS.md and
//! the adjacent tests for fixture outputs.
use control_isolation::{Exec, Isolation, Limits, Logs, Network, RunSpec, Size, Tmpfs, User};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{
    io,
    sync::atomic::{AtomicUsize, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

mod parse;
use parse::{highs, minizinc, smtlib};

const MAX_MODEL_BYTES: usize = 256 * 1024;
const MAX_TIME_LIMIT: u64 = 20;
const DEFAULT_TIME_LIMIT: u64 = 10;
/// Container start and model compilation happen outside the solver's own limit.
const GRACE: Duration = Duration::from_secs(8);
const MEMORY_MB: std::ops::RangeInclusive<u64> = 256..=4096;
const DEFAULT_MEMORY_MB: u64 = 1024;
const CPUS: u32 = 2;
const MAX_CONCURRENT: usize = 2;
const STDOUT_BYTES: usize = 1024 * 1024;
const STDERR_BYTES: usize = 64 * 1024;
const MAX_VARIABLES: usize = 2000;
const VARIABLE_BYTES: usize = 128 * 1024;
const DIAGNOSTIC_CHARS: usize = 4000;
const MAX_MESSAGES: usize = 20;
const REBUILD: &str = "nix run .#setup -- --solver";
/// Writes stdin to the model path ($0). Solvers that only write a result file
/// ($1) run with their log on stderr; the file then becomes standard output.
const STAGE: &str = r#"cat > "$0" || exit 96
result=$1; shift
if [ -z "$result" ]; then exec "$@"; fi
"$@" >&2; code=$?
cat -- "$result" 2>/dev/null
exit $code"#;
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
static SEQUENCE: AtomicUsize = AtomicUsize::new(0);

/// One solver in the registry.
pub struct Solver {
    pub id: &'static str,
    pub summary: &'static str,
    /// Accepted model formats (file extensions); the first is the default.
    pub formats: &'static [&'static str],
    /// A result file the solver writes instead of standard output, if any.
    pub result_file: Option<&'static str>,
    pub command: fn(&Invocation<'_>) -> Vec<String>,
    pub parse: fn(&str) -> Parsed,
}
pub struct Invocation<'a> {
    pub model: &'a str,
    pub time_limit_seconds: u64,
    pub memory_mb: u64,
    pub threads: u32,
}
pub static SOLVERS: &[Solver] = &[
    Solver {
        id: "highs",
        summary: "HiGHS LP/MILP/QP (CPLEX LP or MPS)",
        formats: &["lp", "mps"],
        result_file: Some("/tmp/solution.txt"),
        command: |run| {
            vec!["highs".into(), "--model_file".into(), run.model.into(), "--time_limit".into(), run.time_limit_seconds.to_string(),
                "--solution_file".into(), "/tmp/solution.txt".into()]
        },
        parse: highs,
    },
    Solver {
        id: "z3",
        summary: "Z3 SMT, including (minimize)/(maximize) objectives (SMT-LIB2)",
        formats: &["smt2"],
        result_file: None,
        command: |run| {
            vec!["z3".into(), "-smt2".into(), format!("-t:{}", run.time_limit_seconds * 1000), format!("-T:{}", run.time_limit_seconds + 2),
                format!("-memory:{}", run.memory_mb * 3 / 4), run.model.into()]
        },
        parse: smtlib,
    },
    Solver {
        id: "cp-sat",
        summary: "OR-Tools CP-SAT through MiniZinc (constraint programming, scheduling, assignment)",
        formats: &["mzn"],
        result_file: None,
        command: |run| minizinc_command("cp-sat", run),
        parse: minizinc,
    },
];
/// MiniZinc drives any solver with a FlatZinc backend in the image; a further
/// MiniZinc solver needs only its package and a registry entry using this.
fn minizinc_command(backend: &str, run: &Invocation<'_>) -> Vec<String> {
    let solve_ms = run.time_limit_seconds * 1000;
    vec!["minizinc".into(), "--solver".into(), backend.into(), "--json-stream".into(), "--output-mode".into(), "json".into(),
        "--output-objective".into(), "--solver-time-limit".into(), solve_ms.to_string(), "--time-limit".into(), (solve_ms + 3000).to_string(),
        "-p".into(), run.threads.to_string(), run.model.into()]
}
pub fn lookup(id: &str) -> Option<&'static Solver> {
    SOLVERS.iter().find(|solver| solver.id == id)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Optimal,
    Feasible,
    Infeasible,
    Unbounded,
    InfeasibleOrUnbounded,
    Timeout,
    Unknown,
    Error,
}
impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Optimal => "optimal",
            Self::Feasible => "feasible",
            Self::Infeasible => "infeasible",
            Self::Unbounded => "unbounded",
            Self::InfeasibleOrUnbounded => "infeasible_or_unbounded",
            Self::Timeout => "timeout",
            Self::Unknown => "unknown",
            Self::Error => "error",
        }
    }
}
/// What a parser understood from a solver's standard output.
#[derive(Debug, Default)]
pub struct Parsed {
    /// None when the output contained no verdict at all.
    pub status: Option<Status>,
    pub solver_status: String,
    pub objective: Option<Value>,
    pub variables: Vec<(String, Value)>,
    pub messages: Vec<String>,
    pub extra: Map<String, Value>,
}
/// What the container returned.
pub struct Raw {
    pub stdout: String,
    pub stdout_truncated: bool,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub deadline_exceeded: bool,
    pub elapsed: Duration,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    solver: String,
    model: String,
    format: Option<String>,
    time_limit_seconds: Option<u64>,
    memory_mb: Option<u64>,
    #[serde(default)]
    nonzero_only: bool,
}
/// A validated request.
pub struct Request {
    solver: &'static Solver,
    format: &'static str,
    model: String,
    time_limit_seconds: u64,
    memory_mb: u64,
    nonzero_only: bool,
}
fn fail(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}

pub fn definition() -> Value {
    let solvers: Vec<&str> = SOLVERS.iter().map(|solver| solver.id).collect();
    let catalogue = SOLVERS.iter().map(|solver| format!("{} ({}): {}", solver.id, solver.formats.join("/"), solver.summary)).collect::<Vec<_>>().join("; ");
    json!({"name":"control.solve","description":format!("Solve an optimisation or constraint model with an exact solver instead of reasoning it out in tokens: assignment, scheduling, packing, routing, allocation and consistency checks. Runs in a fresh offline container; returns status (optimal, feasible, infeasible, unbounded, infeasible_or_unbounded, timeout, unknown or error), objective, a bounded variable assignment and solver provenance. Solvers: {catalogue}. Model text up to 256 KiB inline; time limit up to {MAX_TIME_LIMIT} s. Larger or longer solves: control_workspace_start with environment solver. Requires the pinned solver image."),
        "inputSchema":{"type":"object","properties":{
            "solver":{"type":"string","enum":solvers},
            "model":{"type":"string","minLength":1,"maxLength":MAX_MODEL_BYTES},
            "format":{"type":"string","enum":["lp","mps","smt2","mzn"],"description":"Model format; defaults to the solver's first format"},
            "time_limit_seconds":{"type":"integer","minimum":1,"maximum":MAX_TIME_LIMIT,"default":DEFAULT_TIME_LIMIT},
            "memory_mb":{"type":"integer","minimum":*MEMORY_MB.start(),"maximum":*MEMORY_MB.end(),"default":DEFAULT_MEMORY_MB},
            "nonzero_only":{"type":"boolean","default":false,"description":"Omit variables whose value is 0 or false"}},
        "required":["solver","model"],"additionalProperties":false},
        "annotations":{"readOnlyHint":false,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false}})
}

pub fn validate(input: Input) -> io::Result<Request> {
    let solver = lookup(&input.solver).ok_or_else(|| {
        fail(format!("Unknown solver {:?}; available: {}", input.solver, SOLVERS.iter().map(|s| s.id).collect::<Vec<_>>().join(", ")))
    })?;
    let format = match input.format.as_deref() {
        None => solver.formats[0],
        Some(format) => solver.formats.iter().copied().find(|known| *known == format).ok_or_else(|| {
            fail(format!("Solver {} accepts formats {}; got {format:?}", solver.id, solver.formats.join(", ")))
        })?,
    };
    if input.model.trim().is_empty() || input.model.len() > MAX_MODEL_BYTES || input.model.contains('\0') {
        return Err(fail(format!("Model must be non-empty text of at most {MAX_MODEL_BYTES} bytes")));
    }
    let time_limit_seconds = input.time_limit_seconds.unwrap_or(DEFAULT_TIME_LIMIT);
    if !(1..=MAX_TIME_LIMIT).contains(&time_limit_seconds) {
        return Err(fail(format!("time_limit_seconds must be 1–{MAX_TIME_LIMIT}; use control_workspace_start with environment solver for longer solves")));
    }
    let memory_mb = input.memory_mb.unwrap_or(DEFAULT_MEMORY_MB);
    if !MEMORY_MB.contains(&memory_mb) {
        return Err(fail(format!("memory_mb must be {}–{}", MEMORY_MB.start(), MEMORY_MB.end())));
    }
    Ok(Request { solver, format, model: input.model, time_limit_seconds, memory_mb, nonzero_only: input.nonzero_only })
}

/// The operator-pinned solver image, if one is configured.
pub(crate) fn image() -> io::Result<Option<String>> {
    let value = match std::env::var("CONTROL_SOLVER_IMAGE") {
        Ok(value) => Some(value),
        Err(_) => {
            let path = crate::config::directory().map_err(io::Error::other)?.join("solver-image-id");
            match crate::config::read_regular(&path, 256, true) {
                Ok(bytes) => Some(String::from_utf8(bytes).map_err(io::Error::other)?),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(error),
            }
        }
    };
    value
        .map(|value| {
            let value = value.trim().to_owned();
            if !crate::runtime::pinned_image(&value) {
                return Err(fail("Solver image must be pinned by digest"));
            }
            Ok(value)
        })
        .transpose()
}
fn pinned_image() -> io::Result<String> {
    image()?.ok_or_else(|| fail(format!("No pinned solver image. Run `{REBUILD}` to build it and record solver-image-id in the node configuration directory.")))
}
/// The image's `dev.control.solvers` label: solver ID to package and version.
fn label(isolation: &dyn Isolation, image: &str) -> io::Result<String> {
    let mut labels = isolation
        .image_labels(&image.into())
        .map_err(|_| fail(format!("The pinned solver image is not loaded in the container engine; run `{REBUILD}`")))?;
    Ok(labels.remove("dev.control.solvers").unwrap_or_default())
}
fn registry(label: &str) -> io::Result<Map<String, Value>> {
    serde_json::from_str(label.trim()).map_err(|_| fail(format!("The solver image lacks its solver registry label; rebuild it with `{REBUILD}`")))
}
/// Registered solvers the image provides; an error names any it lacks.
pub(crate) fn installed(isolation: &dyn Isolation, image: &str) -> io::Result<String> {
    check_installed(&label(isolation, image)?)
}
pub fn check_installed(label: &str) -> io::Result<String> {
    let solvers = registry(label)?;
    let missing: Vec<&str> = SOLVERS.iter().map(|solver| solver.id).filter(|id| !solvers.contains_key(*id)).collect();
    if !missing.is_empty() {
        return Err(fail(format!("The pinned solver image lacks {}; rebuild it with `{REBUILD}`", missing.join(", "))));
    }
    Ok(SOLVERS.iter().map(|solver| solver.id).collect::<Vec<_>>().join(", "))
}
/// Provenance for one solver from the image's `dev.control.solvers` label.
pub fn provenance(label: &str, image: &str, id: &str) -> io::Result<Value> {
    let solvers = registry(label)?;
    let entry = solvers
        .get(id)
        .and_then(Value::as_object)
        .ok_or_else(|| fail(format!("The pinned solver image does not provide {id}; rebuild it with `{REBUILD}`")))?;
    let mut provenance = Map::new();
    provenance.insert("image".into(), json!(image));
    for key in ["package", "version", "frontend"] {
        if let Some(value) = entry.get(key).and_then(Value::as_str).filter(|v| v.len() <= 200) {
            provenance.insert(key.into(), json!(value));
        }
    }
    Ok(Value::Object(provenance))
}

pub fn call(args: &Value) -> io::Result<Value> {
    let input: Input = serde_json::from_value(args.clone())
        .map_err(|_| fail("Expected solver, model and optional format, time_limit_seconds, memory_mb and nonzero_only"))?;
    let request = validate(input)?;
    let _slot = Slot::acquire()?;
    let image = pinned_image()?;
    let isolation = crate::engine::isolation();
    let provenance = provenance(&label(isolation, &image)?, &image, request.solver.id)?;
    let raw = run(isolation, &image, &request)?;
    Ok(shape(&request, &raw, provenance))
}

/// Bounds concurrent solver containers on this node.
struct Slot;
impl Slot {
    fn acquire() -> io::Result<Self> {
        ACTIVE
            .try_update(Ordering::AcqRel, Ordering::Acquire, |active| (active < MAX_CONCURRENT).then_some(active + 1))
            .map(|_| Self)
            .map_err(|_| fail(format!("Solver tool is busy ({MAX_CONCURRENT} solves running); retry shortly")))
    }
}
impl Drop for Slot {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::AcqRel);
    }
}

fn container(image: &str, name: &str, request: &Request) -> RunSpec {
    let model = format!("/tmp/model.{}", request.format);
    let memory = Size::Mib(request.memory_mb);
    let scratch = (request.memory_mb / 2).min(512);
    let command = (request.solver.command)(&Invocation {
        model: &model,
        time_limit_seconds: request.time_limit_seconds,
        memory_mb: request.memory_mb,
        threads: CPUS,
    });
    RunSpec {
        name: name.into(),
        image: image.into(),
        entrypoint: Some("/bin/bash".into()),
        user: Some(User { uid: 65534, gid: 65534 }),
        workdir: Some("/tmp".into()),
        limits: Limits { pids: Some(128), memory: Some(memory), memory_swap: Some(memory), cpus: Some(u64::from(CPUS)) },
        network: Network::None,
        logs: Logs::None,
        input: control_isolation::Input::Attached,
        ..RunSpec::default()
    }
    .label("dev.control.tool", "solver")
    .tmpfs(Tmpfs { nodev: true, exec: Exec::Deny, ..Tmpfs::new("/tmp", Size::Mib(scratch)) })
    .env("HOME", "/tmp")
    .args(["-c", STAGE, &model, request.solver.result_file.unwrap_or("")])
    .args(command)
}

fn run(isolation: &dyn Isolation, image: &str, request: &Request) -> io::Result<Raw> {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let name = format!("control-solve-{stamp}-{}", SEQUENCE.fetch_add(1, Ordering::Relaxed));
    let finished = crate::engine::ephemeral::run(isolation, crate::engine::ephemeral::Spec {
        run: container(image, &name, request),
        input: request.model.clone().into_bytes(),
        deadline: Duration::from_secs(request.time_limit_seconds) + GRACE,
        stdout_bytes: STDOUT_BYTES,
        stderr_bytes: Some(STDERR_BYTES),
        unavailable: "Could not start the container engine for the solver",
    })?;
    Ok(Raw {
        stdout: String::from_utf8_lossy(&finished.stdout).into_owned(),
        stdout_truncated: finished.stdout_truncated,
        stderr: String::from_utf8_lossy(&finished.stderr).into_owned(),
        exit_code: finished.status.and_then(|status| status.code()),
        deadline_exceeded: finished.status.is_none(),
        elapsed: finished.elapsed,
    })
}
fn clean(text: &str) -> String {
    text.chars().filter(|c| !c.is_control() || *c == '\n' || *c == '\t').collect()
}
fn last_chars(text: &str, limit: usize) -> String {
    let text = clean(text.trim());
    let count = text.chars().count();
    if count <= limit {
        return text;
    }
    format!("[earlier diagnostics omitted]\n{}", text.chars().skip(count - limit).collect::<String>())
}
fn is_zero(value: &Value) -> bool {
    value.as_f64().is_some_and(|v| v.abs() < 1e-9) || *value == Value::Bool(false)
}

/// Combines the parser's view with how the container ended. Pure, so fixture
/// outputs exercise every status without a container.
pub fn shape(request: &Request, raw: &Raw, provenance: Value) -> Value {
    let mut parsed = (request.solver.parse)(&raw.stdout);
    let limit_reached = raw.elapsed >= Duration::from_secs(request.time_limit_seconds);
    let status = if raw.deadline_exceeded {
        parsed.messages.push(format!("Stopped after {} s: the solver did not finish within its time limit plus start-up grace", raw.elapsed.as_secs()));
        Status::Timeout
    } else {
        match (parsed.status, raw.exit_code) {
            (Some(Status::Unknown), _) if limit_reached => Status::Timeout,
            (Some(status), _) => status,
            (None, Some(137)) => {
                parsed.messages.push(format!("The solver was killed, most likely for exceeding memory_mb={}", request.memory_mb));
                Status::Error
            }
            (None, Some(96)) => {
                parsed.messages.push("Could not stage the model inside the solver container".into());
                Status::Error
            }
            (None, Some(125..=127)) => {
                parsed.messages.push(format!("The container engine could not run the solver; rebuild the image with `{REBUILD}`"));
                Status::Error
            }
            (None, Some(0)) if limit_reached => Status::Timeout,
            (None, Some(0)) => {
                parsed.messages.push("The solver returned no verdict; check that the model requests one (for SMT-LIB, (check-sat))".into());
                Status::Unknown
            }
            (None, _) => Status::Error,
        }
    };
    if raw.stdout_truncated {
        parsed.messages.push(format!("Solver output exceeded {STDOUT_BYTES} bytes and was truncated; use nonzero_only or a smaller output"));
    }
    let total = parsed.variables.len();
    let has_solution = parsed.objective.is_some() || total > 0;
    let mut variables = Map::new();
    let (mut bytes, mut truncated, mut omitted_zero) = (0usize, false, 0usize);
    for (name, value) in parsed.variables {
        if request.nonzero_only && is_zero(&value) {
            omitted_zero += 1;
            continue;
        }
        let size = name.len() + value.to_string().len();
        if variables.len() >= MAX_VARIABLES || bytes + size > VARIABLE_BYTES {
            truncated = true;
            break;
        }
        bytes += size;
        variables.insert(name, value);
    }
    let mut messages: Vec<Value> = parsed.messages.iter().take(MAX_MESSAGES).map(|m| json!(last_chars(m, 1000))).collect();
    if parsed.messages.len() > MAX_MESSAGES {
        messages.push(json!(format!("{} more messages omitted", parsed.messages.len() - MAX_MESSAGES)));
    }
    let mut result = json!({
        "solver": request.solver.id,
        "format": request.format,
        "status": status.as_str(),
        "solver_status": parsed.solver_status,
        "has_solution": has_solution,
        "objective": parsed.objective,
        "variables": variables,
        "variable_count": total,
        "variables_returned": variables.len(),
        "variables_truncated": truncated,
        "messages": messages,
        "elapsed_ms": raw.elapsed.as_millis() as u64,
        "exit_code": raw.exit_code,
        "limits": {"time_limit_seconds": request.time_limit_seconds, "memory_mb": request.memory_mb, "cpus": CPUS},
        "provenance": provenance,
    });
    if request.nonzero_only {
        result["variables_omitted_zero"] = json!(omitted_zero);
    }
    if !matches!(status, Status::Optimal | Status::Feasible | Status::Infeasible | Status::Unbounded) && !raw.stderr.trim().is_empty() {
        result["diagnostics"] = json!(last_chars(&raw.stderr, DIAGNOSTIC_CHARS));
    }
    for (key, value) in parsed.extra {
        result.as_object_mut().unwrap().entry(key).or_insert(value);
    }
    result
}

#[cfg(test)]
mod tests;
