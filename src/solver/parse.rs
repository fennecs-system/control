//! Solver output parsers. Each reads one solver's standard output into
//! [`Parsed`]; none trusts its input beyond the bounds the caller applied.
use super::{Parsed, Status};
use serde_json::{Value, json};

/// A JSON number for a solver value: integral values become integers.
pub fn number(value: f64) -> Value {
    if !value.is_finite() {
        return json!(if value.is_nan() { "nan" } else if value > 0.0 { "inf" } else { "-inf" });
    }
    if value.fract() == 0.0 && value.abs() < 9.0e15 {
        json!(value as i64)
    } else {
        json!(value)
    }
}

/// HiGHS `--solution_file` text: model status, then primal values by column.
pub fn highs(text: &str) -> Parsed {
    let mut parsed = Parsed::default();
    let mut lines = text.lines().map(str::trim);
    let mut primal_feasible = false;
    while let Some(line) = lines.next() {
        if line == "Model status" {
            parsed.solver_status = lines.next().unwrap_or("").to_owned();
        } else if line == "# Primal solution values" {
            primal_feasible = lines.next() == Some("Feasible");
            if !primal_feasible {
                continue;
            }
            for line in lines.by_ref() {
                if let Some(objective) = line.strip_prefix("Objective ") {
                    parsed.objective = objective.trim().parse::<f64>().ok().map(number);
                } else if let Some(count) = line.strip_prefix("# Columns ") {
                    let count = count.trim().parse::<usize>().unwrap_or(0);
                    for line in lines.by_ref().take(count) {
                        if let Some((name, value)) = line.rsplit_once(char::is_whitespace) {
                            let value = value.parse::<f64>().map(number).unwrap_or_else(|_| json!(value));
                            parsed.variables.push((name.trim().to_owned(), value));
                        }
                    }
                    break;
                }
            }
        }
    }
    parsed.status = match parsed.solver_status.as_str() {
        "" => None,
        "Optimal" => Some(Status::Optimal),
        "Infeasible" => Some(Status::Infeasible),
        "Unbounded" => Some(Status::Unbounded),
        "Primal infeasible or unbounded" => Some(Status::InfeasibleOrUnbounded),
        "Time limit reached" => Some(Status::Timeout),
        "Bound on objective reached" | "Target for objective reached" | "Solution limit reached" if primal_feasible => Some(Status::Feasible),
        "Iteration limit reached" | "Interrupted by user" | "Unknown" | "Empty" => Some(Status::Unknown),
        _ if parsed.solver_status.ends_with("error") || parsed.solver_status == "Not Set" || parsed.solver_status == "Memory limit reached" => Some(Status::Error),
        _ => Some(Status::Unknown),
    };
    if parsed.status == Some(Status::Infeasible) {
        parsed.objective = None;
        parsed.variables.clear();
    }
    parsed
}

/// MiniZinc `--json-stream`: one JSON object per line. The final solution
/// carries the assignment; `_objective` is the objective value.
pub fn minizinc(text: &str) -> Parsed {
    let mut parsed = Parsed::default();
    let mut solution: Option<serde_json::Map<String, Value>> = None;
    let mut status = None;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            parsed.messages.push(format!("Unparsed output: {}", line.chars().take(300).collect::<String>()));
            continue;
        };
        match message["type"].as_str() {
            Some("solution") => solution = message["output"]["json"].as_object().cloned(),
            Some("status") => status = message["status"].as_str().map(str::to_owned),
            Some(kind @ ("error" | "warning")) => {
                let location = if message["location"].is_object() { &message["location"] } else { &message["stack"][0]["location"] };
                let line = location["firstLine"].as_u64().map(|line| format!(" (line {line})")).unwrap_or_default();
                let what = message["what"].as_str().map(|what| format!("{what}: ")).unwrap_or_default();
                parsed.messages.push(format!("{kind}: {what}{}{line}", message["message"].as_str().unwrap_or("")));
                if kind == "error" {
                    status.get_or_insert_with(|| "ERROR".to_owned());
                }
            }
            _ => {}
        }
    }
    let optimising = solution.as_ref().is_some_and(|s| s.contains_key("_objective"));
    if let Some(solution) = solution {
        for (name, value) in solution {
            if name == "_objective" {
                parsed.objective = Some(value);
            } else if name != "_checker" {
                parsed.variables.push((name, value));
            }
        }
    }
    // A satisfaction model reports its solution without a separate status line.
    let status = status.or_else(|| parsed.variables.first().map(|_| "SATISFIED".to_owned()));
    parsed.status = status.as_deref().map(|status| match status {
        "OPTIMAL_SOLUTION" => Status::Optimal,
        "ALL_SOLUTIONS" => Status::Feasible,
        // An optimisation only stops short of a proof at its time limit.
        "SATISFIED" if optimising => Status::Timeout,
        "SATISFIED" => Status::Feasible,
        "UNSATISFIABLE" => Status::Infeasible,
        "UNBOUNDED" => Status::Unbounded,
        "UNSAT_OR_UNBOUNDED" => Status::InfeasibleOrUnbounded,
        "ERROR" => Status::Error,
        _ => Status::Unknown,
    });
    parsed.solver_status = status.unwrap_or_default();
    parsed
}

#[derive(Clone, Debug, PartialEq)]
pub enum Sexp {
    Atom(String),
    List(Vec<Sexp>),
}
impl Sexp {
    fn atom(&self) -> Option<&str> {
        match self {
            Self::Atom(atom) => Some(atom),
            Self::List(_) => None,
        }
    }
    fn head(&self) -> Option<&str> {
        match self {
            Self::List(items) => items.first().and_then(Sexp::atom),
            Self::Atom(_) => None,
        }
    }
    pub fn render(&self) -> String {
        match self {
            Self::Atom(atom) => atom.clone(),
            Self::List(items) => format!("({})", items.iter().map(Sexp::render).collect::<Vec<_>>().join(" ")),
        }
    }
}
const MAX_DEPTH: usize = 256;
/// Reads a sequence of SMT-LIB S-expressions iteratively (bounded depth, no
/// recursion on input). Unbalanced input yields what was complete.
pub fn sexps(text: &str) -> Vec<Sexp> {
    let mut stack: Vec<Vec<Sexp>> = vec![Vec::new()];
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '(' if stack.len() <= MAX_DEPTH => stack.push(Vec::new()),
            '(' => return stack.swap_remove(0),
            ')' if stack.len() > 1 => {
                let list = stack.pop().unwrap();
                stack.last_mut().unwrap().push(Sexp::List(list));
            }
            ')' => {}
            ';' => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            c if c.is_whitespace() => {}
            '"' | '|' => {
                let mut atom = String::from(c);
                while let Some(next) = chars.next() {
                    atom.push(next);
                    if next == c {
                        // SMT-LIB escapes a quote inside a string by doubling it.
                        if c == '"' && chars.peek() == Some(&'"') {
                            atom.push(chars.next().unwrap());
                            continue;
                        }
                        break;
                    }
                }
                stack.last_mut().unwrap().push(Sexp::Atom(atom));
            }
            c => {
                let mut atom = String::from(c);
                while let Some(&next) = chars.peek() {
                    if next.is_whitespace() || matches!(next, '(' | ')' | ';' | '"') {
                        break;
                    }
                    atom.push(next);
                    chars.next();
                }
                stack.last_mut().unwrap().push(Sexp::Atom(atom));
            }
        }
    }
    stack.swap_remove(0)
}
/// A JSON value for a model value: numbers, booleans and simple negations or
/// ratios become JSON; anything else (bit-vectors, arrays, functions) stays text.
pub fn smt_value(value: &Sexp) -> Value {
    fn numeric(value: &Sexp) -> Option<f64> {
        match value {
            Sexp::Atom(atom) => atom.parse::<f64>().ok().filter(|_| atom.bytes().all(|b| b.is_ascii_digit() || b == b'.')),
            Sexp::List(items) => match items.as_slice() {
                [op, x] if op.atom() == Some("-") => numeric(x).map(|x| -x),
                [op, a, b] if op.atom() == Some("/") => Some(numeric(a)? / numeric(b)?),
                _ => None,
            },
        }
    }
    match value {
        Sexp::Atom(atom) if atom == "true" => json!(true),
        Sexp::Atom(atom) if atom == "false" => json!(false),
        Sexp::Atom(atom) if !atom.is_empty() && atom.bytes().all(|b| b.is_ascii_digit()) => {
            atom.parse::<i64>().map(|v| json!(v)).unwrap_or_else(|_| json!(atom))
        }
        Sexp::List(items) if matches!(items.as_slice(), [op, Sexp::Atom(x)] if op.atom() == Some("-") && x.bytes().all(|b| b.is_ascii_digit())) => {
            let digits = items[1].atom().unwrap();
            digits.parse::<i64>().map(|v| json!(-v)).unwrap_or_else(|_| json!(value.render()))
        }
        _ => numeric(value).filter(|v| v.is_finite()).map(number).unwrap_or_else(|| json!(value.render())),
    }
}
fn define_fun(item: &Sexp) -> Option<(String, Value)> {
    let Sexp::List(parts) = item else { return None };
    match parts.as_slice() {
        [head, name, Sexp::List(args), _sort, body] if head.atom() == Some("define-fun") => {
            let name = name.atom()?.to_owned();
            Some(if args.is_empty() { (name, smt_value(body)) } else { (name, json!(item.render())) })
        }
        _ => None,
    }
}
/// SMT-LIB2 responses from `(check-sat)`, `(get-objectives)`, `(get-model)` and
/// `(get-value ...)`; `(error ...)` responses become messages.
pub fn smtlib(text: &str) -> Parsed {
    let mut parsed = Parsed::default();
    let mut verdict = None;
    let mut objectives = Vec::new();
    for item in sexps(text) {
        match &item {
            Sexp::Atom(atom) if matches!(atom.as_str(), "sat" | "unsat" | "unknown" | "timeout") => verdict = Some(atom.clone()),
            Sexp::Atom(_) => {}
            Sexp::List(items) => match item.head() {
                Some("error") => parsed.messages.push(format!("error: {}", items.get(1).map(Sexp::render).unwrap_or_default().trim_matches('"'))),
                Some("objectives") => {
                    for objective in &items[1..] {
                        if let Sexp::List(pair) = objective
                            && let [term, value] = pair.as_slice()
                        {
                            objectives.push((term.render(), value.clone()));
                        }
                    }
                }
                Some("model") => parsed.variables.extend(items[1..].iter().filter_map(define_fun)),
                _ if !items.is_empty() && items.iter().all(|i| i.head() == Some("define-fun")) => {
                    // A later model replaces an earlier one.
                    parsed.variables = items.iter().filter_map(define_fun).collect();
                }
                _ => {
                    for pair in items {
                        if let Sexp::List(pair) = pair
                            && let [term, value] = pair.as_slice()
                        {
                            parsed.variables.push((term.render(), smt_value(value)));
                        }
                    }
                }
            },
        }
    }
    let unbounded = objectives.iter().any(|(_, value)| value.render().contains("oo"));
    if let Some((_, value)) = objectives.first() {
        parsed.objective = Some(smt_value(value));
    }
    if objectives.len() > 1 {
        parsed.extra.insert(
            "objectives".into(),
            Value::Array(objectives.iter().map(|(term, value)| json!({"term": term, "value": smt_value(value)})).collect()),
        );
    }
    parsed.status = verdict.as_deref().map(|verdict| match verdict {
        "sat" if unbounded => Status::Unbounded,
        "sat" if !objectives.is_empty() => Status::Optimal,
        "sat" => Status::Feasible,
        "unsat" => Status::Infeasible,
        "timeout" => Status::Timeout,
        _ => Status::Unknown,
    });
    if parsed.status.is_none() && !parsed.messages.is_empty() {
        parsed.status = Some(Status::Error);
    }
    parsed.solver_status = verdict.unwrap_or_default();
    parsed
}
