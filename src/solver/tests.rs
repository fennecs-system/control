use super::parse::{Sexp, sexps, smt_value};
use super::*;

const HIGHS_OPTIMAL: &str = "Model status\nOptimal\n\n# Primal solution values\nFeasible\nObjective 34\n# Columns 2\nx 6\ny 4\n# Rows 3\nc1 14\nc2 14\nc3 2\n\n# Dual solution values\nNone\n\n# Basis\nHiGHS_basis_file v2\nNone\n";
const HIGHS_INFEASIBLE: &str = "Model status\nInfeasible\n\n# Primal solution values\nNone\n\n# Dual solution values\nNone\n\n# Basis\nHiGHS_basis_file v2\nNone\n";
const HIGHS_UNBOUNDED: &str = "Model status\nUnbounded\n\n# Primal solution values\nFeasible\nObjective 1\n# Columns 2\nx 1\ny 0\n# Rows 1\nc1 1\n\n# Dual solution values\nInfeasible\n# Columns 2\nx 0\ny 2\n";
const HIGHS_TIME_LIMIT: &str = "Model status\nTime limit reached\n\n# Primal solution values\nFeasible\nObjective 41.5\n# Columns 3\nassign_1_2 1\nassign_2_1 0\nload 2.5\n# Rows 0\n";
const Z3_OPTIMAL: &str = "sat\n(objectives\n ((+ (* 3 x) (* 4 y)) 34)\n)\n(\n  (define-fun x () Int\n    6)\n  (define-fun b () Bool\n    true)\n  (define-fun y () Int\n    4)\n  (define-fun r () Real\n    (/ 3.0 2.0))\n)\n";
const Z3_UNSAT: &str = "unsat\n(error \"line 5 column 10: model is not available\")\n";
const Z3_SYNTAX: &str = "(error \"line 3 column 0: invalid assert command, ')' expected\")\n";
const Z3_FUNCTION: &str = "sat\n(\n  (define-fun x () Int\n    0)\n  (define-fun f ((x!0 Int)) Int\n    1)\n)\n";
const MZN_OPTIMAL: &str = "{\"type\": \"solution\", \"output\": {\"json\": {  \"x\" : 6,  \"y\" : 4,  \"_objective\" : 34}}, \"sections\": [\"json\"]}\n{\"type\": \"status\", \"status\": \"OPTIMAL_SOLUTION\"}\n";
const MZN_UNSAT: &str = "{\"type\": \"warning\", \"stack\": [{\"location\": {\"filename\": \"/tmp/model.mzn\", \"firstLine\": 2, \"firstColumn\": 12, \"lastLine\": 2, \"lastColumn\": 16}, \"isCompIter\": false, \"description\": \"binary '>' operator expression\"}], \"message\": \"model inconsistency detected\"}\n{\"type\": \"status\", \"status\": \"UNSATISFIABLE\"}\n";
const MZN_SYNTAX: &str = "{\"type\": \"error\", \"what\": \"syntax error\", \"location\": {\"filename\": \"/tmp/model.mzn\", \"firstLine\": 2, \"firstColumn\": 1, \"lastLine\": 2, \"lastColumn\": 10}, \"message\": \"unexpected item, expecting ';' or end of file\"}\n";
const MZN_SATISFY: &str =
    "{\"type\": \"solution\", \"output\": {\"json\": {  \"a\" : [2, 3, 1]}}, \"sections\": [\"json\"]}\n";
const MZN_UNKNOWN: &str = "{\"type\": \"status\", \"status\": \"UNKNOWN\"}\n";

fn request(solver: &str, time_limit_seconds: u64, nonzero_only: bool) -> Request {
    validate(Input {
        solver: solver.into(),
        model: "model".into(),
        format: None,
        time_limit_seconds: Some(time_limit_seconds),
        memory_mb: None,
        nonzero_only,
    })
    .unwrap()
}
fn raw(stdout: &str, exit_code: Option<i32>, elapsed_ms: u64) -> Raw {
    Raw {
        stdout: stdout.into(),
        stdout_truncated: false,
        stderr: String::new(),
        exit_code,
        deadline_exceeded: false,
        elapsed: Duration::from_millis(elapsed_ms),
    }
}
fn solve(solver: &str, stdout: &str) -> Value {
    shape(
        &request(solver, 10, false),
        &raw(stdout, Some(0), 200),
        json!({"image":"sha256:test"}),
    )
}

#[test]
fn registry_ids_formats_and_schema_agree() {
    let mut ids = std::collections::BTreeSet::new();
    let definition = definition();
    let schema = &definition["inputSchema"]["properties"];
    for solver in SOLVERS {
        assert!(ids.insert(solver.id), "duplicate solver {}", solver.id);
        assert!(!solver.formats.is_empty() && !solver.summary.is_empty());
        assert!(std::ptr::eq(lookup(solver.id).unwrap(), solver));
        assert!(schema["solver"]["enum"].as_array().unwrap().contains(&json!(solver.id)));
        for format in solver.formats {
            assert!(
                schema["format"]["enum"].as_array().unwrap().contains(&json!(format)),
                "{format} missing from schema"
            );
        }
        let command = (solver.command)(&Invocation {
            model: "/tmp/model.x",
            time_limit_seconds: 7,
            memory_mb: 1024,
            threads: 2,
        });
        assert!(
            command.iter().any(|arg| arg == "/tmp/model.x"),
            "{} ignores the model path",
            solver.id
        );
        assert!(
            command.iter().any(|arg| arg.contains('7')),
            "{} ignores the time limit",
            solver.id
        );
    }
    assert!(definition["description"].as_str().unwrap().contains("cp-sat (mzn)"));
    assert!(lookup("gurobi").is_none());
}

#[test]
fn requests_are_validated_before_any_container_runs() {
    let input = |solver: &str, format: Option<&str>, model: &str, time: Option<u64>, memory: Option<u64>| Input {
        solver: solver.into(),
        model: model.into(),
        format: format.map(str::to_owned),
        time_limit_seconds: time,
        memory_mb: memory,
        nonzero_only: false,
    };
    let ok = validate(input("highs", None, "x", None, None)).unwrap();
    assert_eq!(
        (ok.format, ok.time_limit_seconds, ok.memory_mb),
        ("lp", DEFAULT_TIME_LIMIT, DEFAULT_MEMORY_MB)
    );
    assert_eq!(
        validate(input("highs", Some("mps"), "x", Some(20), Some(4096)))
            .unwrap()
            .format,
        "mps"
    );
    let error = |input| validate(input).err().unwrap().to_string();
    assert!(error(input("cplex", None, "x", None, None)).contains("available: highs, z3, cp-sat"));
    assert!(error(input("z3", Some("lp"), "x", None, None)).contains("accepts formats smt2"));
    assert!(error(input("z3", None, "  \n", None, None)).contains("non-empty"));
    assert!(error(input("z3", None, "a\0b", None, None)).contains("non-empty"));
    assert!(error(input("z3", None, &"x".repeat(MAX_MODEL_BYTES + 1), None, None)).contains("at most"));
    assert!(error(input("z3", None, "x", Some(0), None)).contains("time_limit_seconds"));
    assert!(error(input("z3", None, "x", Some(MAX_TIME_LIMIT + 1), None)).contains("environment solver"));
    assert!(error(input("z3", None, "x", None, Some(128))).contains("memory_mb"));
    // Unknown fields (such as mounts or images) are refused at decoding.
    assert!(call(&json!({"solver":"z3","model":"(check-sat)","image":"alpine"})).is_err());
    assert!(call(&json!({"solver":"z3"})).is_err());
}

#[test]
fn container_arguments_are_isolated_and_bounded() {
    let request = request("highs", 5, false);
    let arguments = arguments("sha256:abc", "control-solve-1", &request);
    for expected in [
        "--network=none",
        "--read-only",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges",
        "--memory=1024m",
        "--memory-swap=1024m",
        "--cpus=2",
        "--pids-limit=128",
        "--pull=never",
        "--rm",
        "--log-driver=none",
    ] {
        assert!(arguments.iter().any(|arg| arg == expected), "missing {expected}");
    }
    assert!(arguments.windows(2).any(|pair| pair == ["--user", "65534:65534"]));
    assert!(!arguments.iter().any(|arg| arg.starts_with("--mount")
        || arg.starts_with("--volume")
        || arg == "-v"
        || arg.starts_with("--privileged")));
    let image = arguments.iter().position(|arg| arg == "sha256:abc").unwrap();
    assert_eq!(
        &arguments[image + 1..image + 5],
        ["-c", STAGE, "/tmp/model.lp", "/tmp/solution.txt"]
    );
    assert_eq!(arguments[image + 5], "highs");
    let z3 = arguments_for("z3");
    assert_eq!(
        z3[z3.iter().position(|arg| arg == "/tmp/model.smt2").unwrap() + 1],
        "",
        "z3 writes its result to stdout"
    );
    assert!(z3.iter().any(|arg| arg == "-T:7") && z3.iter().any(|arg| arg == "-memory:768"));
    let minizinc = arguments_for("cp-sat");
    assert!(minizinc.windows(2).any(|pair| pair == ["--solver", "cp-sat"]));
    assert!(minizinc.windows(2).any(|pair| pair == ["--solver-time-limit", "5000"]));
}
fn arguments(image: &str, name: &str, request: &Request) -> Vec<String> {
    crate::engine::golden::run(&container(image, name, request))
}
fn arguments_for(solver: &str) -> Vec<String> {
    arguments("sha256:abc", "control-solve-1", &request(solver, 5, false))
}

#[test]
fn highs_solution_files_map_to_statuses_and_assignments() {
    let optimal = solve("highs", HIGHS_OPTIMAL);
    assert_eq!(optimal["status"], "optimal");
    assert_eq!(optimal["solver_status"], "Optimal");
    assert_eq!(optimal["objective"], 34);
    assert_eq!(optimal["variables"], json!({"x":6,"y":4}));
    assert_eq!(optimal["variable_count"], 2);
    assert_eq!(optimal["has_solution"], true);
    assert!(optimal.get("diagnostics").is_none());

    let infeasible = solve("highs", HIGHS_INFEASIBLE);
    assert_eq!(
        (infeasible["status"].as_str(), infeasible["has_solution"].as_bool()),
        (Some("infeasible"), Some(false))
    );
    assert_eq!(infeasible["objective"], Value::Null);
    let unbounded = solve("highs", HIGHS_UNBOUNDED);
    assert_eq!(unbounded["status"], "unbounded");
    assert_eq!(
        unbounded["variables"],
        json!({"x":1,"y":0}),
        "dual columns are not primal values"
    );
    let timeout = solve("highs", HIGHS_TIME_LIMIT);
    assert_eq!(
        (timeout["status"].as_str(), timeout["has_solution"].as_bool()),
        (Some("timeout"), Some(true))
    );
    assert_eq!(timeout["objective"], 41.5);
    assert_eq!(timeout["variables"]["load"], 2.5);
    let other = super::parse::highs("Model status\nPrimal infeasible or unbounded\n");
    assert_eq!(other.status, Some(Status::InfeasibleOrUnbounded));
    assert_eq!(
        super::parse::highs("Model status\nLoad error\n").status,
        Some(Status::Error)
    );
    assert_eq!(super::parse::highs("").status, None);
}

#[test]
fn highs_load_errors_report_engine_diagnostics() {
    let mut output = raw("", Some(255), 50);
    output.stderr = "Running HiGHS 1.14.0\nERROR:   Parser error reading /tmp/model.lp\nError loading file\n".into();
    let result = shape(&request("highs", 10, false), &output, json!({}));
    assert_eq!(result["status"], "error");
    assert!(
        result["diagnostics"]
            .as_str()
            .unwrap()
            .contains("Parser error reading /tmp/model.lp")
    );
    assert_eq!(result["exit_code"], 255);
}

#[test]
fn smtlib_responses_cover_models_objectives_and_errors() {
    let optimal = solve("z3", Z3_OPTIMAL);
    assert_eq!(optimal["status"], "optimal");
    assert_eq!(optimal["solver_status"], "sat");
    assert_eq!(optimal["objective"], 34);
    assert_eq!(optimal["variables"], json!({"x":6,"y":4,"b":true,"r":1.5}));

    let unsat = solve("z3", Z3_UNSAT);
    assert_eq!(unsat["status"], "infeasible");
    assert!(
        unsat["messages"][0]
            .as_str()
            .unwrap()
            .contains("model is not available")
    );
    let syntax = shape(&request("z3", 10, false), &raw(Z3_SYNTAX, Some(1), 30), json!({}));
    assert_eq!(syntax["status"], "error");
    assert!(syntax["messages"][0].as_str().unwrap().contains("')' expected"));

    let function = solve("z3", Z3_FUNCTION);
    assert_eq!(function["status"], "feasible");
    assert_eq!(function["variables"]["x"], 0);
    assert_eq!(function["variables"]["f"], "(define-fun f ((x!0 Int)) Int 1)");

    assert_eq!(solve("z3", "timeout\n")["status"], "timeout");
    assert_eq!(solve("z3", "unknown\n")["status"], "unknown");
    let late = shape(&request("z3", 1, false), &raw("unknown\n", Some(0), 1500), json!({}));
    assert_eq!(late["status"], "timeout", "unknown at the time limit is a timeout");
    let unbounded = solve("z3", "sat\n(objectives\n ((+ x y) oo)\n)\n");
    assert_eq!(unbounded["status"], "unbounded");
    let values = solve("z3", "sat\n((x (- 5)) (y (/ 1.0 4.0)) ((select a 1) #x0f))\n");
    assert_eq!(values["variables"], json!({"x":-5,"y":0.25,"(select a 1)":"#x0f"}));
    let both = solve("z3", "sat\n(objectives\n ((+ x y) 3)\n (z (- 2))\n)\n");
    assert_eq!(both["objective"], 3);
    assert_eq!(both["objectives"][1], json!({"term":"z","value":-2}));
    let none = solve("z3", "");
    assert_eq!(none["status"], "unknown");
    assert!(none["messages"][0].as_str().unwrap().contains("(check-sat)"));
}

#[test]
fn sexp_reader_handles_strings_comments_and_depth() {
    assert_eq!(
        sexps("(a \"b ) \"\"c\" |d e|) ; (ignored\n x"),
        vec![
            Sexp::List(vec![
                Sexp::Atom("a".into()),
                Sexp::Atom("\"b ) \"\"c\"".into()),
                Sexp::Atom("|d e|".into())
            ]),
            Sexp::Atom("x".into()),
        ]
    );
    assert_eq!(sexps(")) sat"), vec![Sexp::Atom("sat".into())]);
    // Deep or unbalanced input never recurses or panics.
    let deep = "(".repeat(100_000);
    assert!(sexps(&deep).is_empty());
    assert!(sexps(&format!("sat {}", "(".repeat(10))).contains(&Sexp::Atom("sat".into())));
    assert_eq!(
        smt_value(&Sexp::Atom("123456789012345678901234567890".into())),
        json!("123456789012345678901234567890")
    );
    assert_eq!(smt_value(&Sexp::Atom("2.5".into())), json!(2.5));
}

#[test]
fn minizinc_streams_map_to_statuses_and_assignments() {
    let optimal = solve("cp-sat", MZN_OPTIMAL);
    assert_eq!(optimal["status"], "optimal");
    assert_eq!(optimal["solver_status"], "OPTIMAL_SOLUTION");
    assert_eq!(optimal["objective"], 34);
    assert_eq!(optimal["variables"], json!({"x":6,"y":4}));
    let unsat = solve("cp-sat", MZN_UNSAT);
    assert_eq!(unsat["status"], "infeasible");
    assert!(
        unsat["messages"][0]
            .as_str()
            .unwrap()
            .contains("model inconsistency detected (line 2)")
    );
    let syntax = shape(&request("cp-sat", 10, false), &raw(MZN_SYNTAX, Some(1), 30), json!({}));
    assert_eq!(syntax["status"], "error");
    assert_eq!(
        syntax["messages"][0],
        "error: syntax error: unexpected item, expecting ';' or end of file (line 2)"
    );
    let satisfy = solve("cp-sat", MZN_SATISFY);
    assert_eq!(satisfy["status"], "feasible");
    assert_eq!(satisfy["variables"]["a"], json!([2, 3, 1]));
    assert_eq!(solve("cp-sat", MZN_UNKNOWN)["status"], "unknown");
    let late = shape(
        &request("cp-sat", 1, false),
        &raw(MZN_UNKNOWN, Some(0), 1200),
        json!({}),
    );
    assert_eq!(late["status"], "timeout");
    let interrupted = solve("cp-sat", &MZN_OPTIMAL.replace("OPTIMAL_SOLUTION", "SATISFIED"));
    assert_eq!(
        (interrupted["status"].as_str(), interrupted["has_solution"].as_bool()),
        (Some("timeout"), Some(true))
    );
    assert_eq!(
        solve("cp-sat", "{\"type\": \"status\", \"status\": \"UNSAT_OR_UNBOUNDED\"}\n")["status"],
        "infeasible_or_unbounded"
    );
    assert!(
        solve("cp-sat", "not json\n")["messages"][0]
            .as_str()
            .unwrap()
            .starts_with("Unparsed output")
    );
}

#[test]
fn container_endings_override_parsed_output() {
    let mut killed = raw("", None, 18_000);
    killed.deadline_exceeded = true;
    let result = shape(&request("highs", 10, false), &killed, json!({}));
    assert_eq!(result["status"], "timeout");
    assert!(result["messages"][0].as_str().unwrap().contains("did not finish"));
    let oom = shape(&request("z3", 10, false), &raw("", Some(137), 900), json!({}));
    assert_eq!(oom["status"], "error");
    assert!(oom["messages"][0].as_str().unwrap().contains("memory_mb=1024"));
    let engine = shape(&request("z3", 10, false), &raw("", Some(125), 100), json!({}));
    assert!(engine["messages"][0].as_str().unwrap().contains("--solver"));
    let stage = shape(&request("z3", 10, false), &raw("", Some(96), 100), json!({}));
    assert!(stage["messages"][0].as_str().unwrap().contains("stage the model"));
    let mut truncated = raw(Z3_OPTIMAL, Some(0), 100);
    truncated.stdout_truncated = true;
    let result = shape(&request("z3", 10, false), &truncated, json!({}));
    assert!(
        result["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m.as_str().unwrap().contains("truncated"))
    );
}

#[test]
fn assignments_are_bounded_and_optionally_sparse() {
    let mut columns =
        String::from("Model status\nOptimal\n\n# Primal solution values\nFeasible\nObjective 1\n# Columns 5000\n");
    for i in 0..5000 {
        columns.push_str(&format!("x{i} {}\n", i % 2));
    }
    let full = solve("highs", &columns);
    assert_eq!(
        (full["variable_count"].as_u64(), full["variables_returned"].as_u64()),
        (Some(5000), Some(MAX_VARIABLES as u64))
    );
    assert_eq!(full["variables_truncated"], true);
    let sparse = shape(&request("highs", 10, true), &raw(&columns, Some(0), 100), json!({}));
    assert_eq!(sparse["variables_returned"], 2000);
    assert_eq!(sparse["variables_truncated"], true);
    assert!(sparse["variables"].as_object().unwrap().values().all(|v| v == 1));
    let small = shape(&request("z3", 10, true), &raw(Z3_FUNCTION, Some(0), 100), json!({}));
    assert_eq!(small["variables_omitted_zero"], 1);
    assert_eq!(small["variables_truncated"], false);
    // Large values are bounded by bytes as well as count.
    let wide = format!(
        "{{\"type\": \"solution\", \"output\": {{\"json\": {{\"a\": [{}], \"b\": 1}}}}}}\n",
        vec!["123456"; 40_000].join(",")
    );
    let result = solve("cp-sat", &wide);
    assert_eq!(
        (
            result["variables_returned"].as_u64(),
            result["variables_truncated"].as_bool()
        ),
        (Some(0), Some(true))
    );
}

#[test]
fn provenance_comes_from_the_image_registry_label() {
    let label = r#"{"cp-sat":{"frontend":"minizinc 2.10.1","package":"or-tools","version":"9.15"},"highs":{"package":"highs","version":"1.14.0"},"z3":{"package":"z3","version":"5.1.0"}}"#;
    assert_eq!(
        provenance(label, "sha256:abc", "cp-sat").unwrap(),
        json!({"image":"sha256:abc","package":"or-tools","version":"9.15","frontend":"minizinc 2.10.1"})
    );
    assert_eq!(check_installed(label).unwrap(), "highs, z3, cp-sat");
    let missing = r#"{"highs":{"package":"highs","version":"1.14.0"}}"#;
    assert!(
        provenance(missing, "sha256:abc", "z3")
            .unwrap_err()
            .to_string()
            .contains("does not provide z3")
    );
    assert!(
        check_installed(missing)
            .unwrap_err()
            .to_string()
            .contains("lacks z3, cp-sat")
    );
    assert!(
        provenance("", "sha256:abc", "z3")
            .unwrap_err()
            .to_string()
            .contains("registry label")
    );
}

#[test]
fn diagnostics_keep_their_end() {
    // The bounded readers themselves are tested with the shared runner (engine::ephemeral).
    assert!(last_chars(&"y".repeat(5000), 100).starts_with("[earlier diagnostics omitted]"));
}

/// Runs each registered solver on a tiny model in its real container.
#[test]
#[ignore = "requires the pinned Nix solver image (nix run .#setup -- --solver) or CONTROL_SOLVER_IMAGE; no model calls"]
fn live_solvers_run_tiny_models_in_their_containers() {
    let lp = "Maximize\n obj: 3 x + 4 y\nSubject To\n c1: x + 2 y <= 14\n c2: 3 x - y >= 0\n c3: x - y <= 2\nBounds\n 0 <= x <= 10\n 0 <= y <= 10\nGeneral\n x y\nEnd\n";
    let smt = "(declare-const x Int)\n(declare-const y Int)\n(assert (and (>= x 0) (<= x 10) (>= y 0) (<= y 10)))\n(assert (<= (+ x (* 2 y)) 14))\n(assert (>= (- (* 3 x) y) 0))\n(assert (<= (- x y) 2))\n(maximize (+ (* 3 x) (* 4 y)))\n(check-sat)\n(get-objectives)\n(get-model)\n";
    let mzn = "var 0..10: x; var 0..10: y;\nconstraint x + 2*y <= 14; constraint 3*x - y >= 0; constraint x - y <= 2;\nsolve maximize 3*x + 4*y;\n";
    for (solver, model) in [("highs", lp), ("z3", smt), ("cp-sat", mzn)] {
        let result = call(&json!({"solver":solver,"model":model,"time_limit_seconds":5})).unwrap();
        assert_eq!(result["status"], "optimal", "{solver}: {result}");
        assert_eq!(result["objective"], 34, "{solver}: {result}");
        assert_eq!(
            (result["variables"]["x"].as_i64(), result["variables"]["y"].as_i64()),
            (Some(6), Some(4)),
            "{solver}: {result}"
        );
        assert!(result["provenance"]["version"].is_string(), "{solver}: {result}");
    }
    let infeasible = call(
        &json!({"solver":"highs","model":"Minimize\n obj: x\nSubject To\n c1: x >= 5\nBounds\n 0 <= x <= 3\nEnd\n"}),
    )
    .unwrap();
    assert_eq!(infeasible["status"], "infeasible");
    let error = call(&json!({"solver":"cp-sat","model":"var 0..3: x\nconstraint x > 5;\n"})).unwrap();
    assert_eq!(error["status"], "error");
    assert!(
        error["messages"][0].as_str().unwrap().contains("syntax error"),
        "{error}"
    );
    // Fermat's cubic case has no positive solution; Z3 cannot prove it in a second.
    let hard = "(declare-const x Int)(declare-const y Int)(declare-const z Int)(assert (> x 0))(assert (> y 0))(assert (> z 0))(assert (= (+ (* x x x) (* y y y)) (* z z z)))(check-sat)";
    let timeout = call(&json!({"solver":"z3","model":hard,"time_limit_seconds":1})).unwrap();
    assert_eq!(timeout["status"], "timeout", "{timeout}");
    assert!(
        timeout["elapsed_ms"].as_u64().unwrap() < 1000 * (1 + GRACE.as_secs()),
        "{timeout}"
    );
}

#[test]
fn solver_launch_vector_is_pinned() {
    use crate::engine::golden::{fill, pin_equivalent};
    pin_equivalent(
        &arguments("sha256:img", "control-solve-1", &request("highs", 5, false)),
        &fill(GOLDEN, &[("stage", STAGE)]),
    );
}
const GOLDEN: &[&str] = &[
    "run",
    "--rm",
    "--interactive",
    "--pull=never",
    "--name",
    "control-solve-1",
    "--label",
    "dev.control.tool=solver",
    "--network=none",
    "--user=65534:65534",
    "--read-only",
    "--cap-drop=ALL",
    "--security-opt=no-new-privileges",
    "--memory=1024m",
    "--memory-swap=1024m",
    "--cpus=2",
    "--pids-limit=128",
    "--tmpfs",
    "/tmp:rw,nosuid,nodev,noexec,size=512m",
    "--env",
    "HOME=/tmp",
    "--workdir=/tmp",
    "--log-driver=none",
    "--entrypoint=/bin/bash",
    "sha256:img",
    "-c",
    "{stage}",
    "/tmp/model.lp",
    "/tmp/solution.txt",
    "highs",
    "--model_file",
    "/tmp/model.lp",
    "--time_limit",
    "5",
    "--solution_file",
    "/tmp/solution.txt",
];
