//! Bounded std-only microbenchmarks in the existing credential-free playground.
use serde::Deserialize;
use serde_json::{json, Value};
use std::io;

pub fn definition() -> Value {
    json!({"name":"control.rust.bench","description":"Benchmark a standalone Rust expression with optional setup code in a fresh offline Nix container. Optimized Rust 2024, standard library only; setup runs once, warmup then repeated samples. Use std::hint::black_box for inputs and return the measured result. Compilation/startup are excluded from sample timings. Combined code/setup ≤14000 bytes. 3-second program deadline; no crates, host files, credentials or network. Timings are workload evidence, not a correctness guarantee.","inputSchema":{"type":"object","properties":{"code":{"type":"string","minLength":1,"maxLength":12000},"setup":{"type":"string","maxLength":12000},"iterations":{"type":"integer","minimum":1,"maximum":1000000,"default":1000},"samples":{"type":"integer","minimum":3,"maximum":31,"default":9}},"required":["code"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":false}})
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    code: String,
    #[serde(default)]
    setup: String,
    #[serde(default = "iterations")]
    iterations: usize,
    #[serde(default = "samples")]
    samples: usize,
}
fn iterations() -> usize {
    1000
}
fn samples() -> usize {
    9
}
fn source(input: &Input) -> io::Result<String> {
    if input.code.trim().is_empty()
        || input.code.len() > 12000
        || input.setup.len() > 12000
        || input.code.len() + input.setup.len() > 14000
        || !(1..=1_000_000).contains(&input.iterations)
        || !(3..=31).contains(&input.samples)
    {
        return Err(io::Error::other(
            "Expected code/setup ≤14000 combined bytes, 1–1000000 iterations and 3–31 samples",
        ));
    }
    // Old images must fail explicitly instead of reporting unoptimized timings.
    Ok(format!(
        r#"#[cfg(debug_assertions)] compile_error!("Benchmark requires the updated optimized playground image; rebuild worker-image.");
fn main() {{
{setup}
#[allow(unused_mut)] let mut __control_operation=|| {{ {code} }};
for _ in 0..{warmup} {{ std::hint::black_box(__control_operation()); }}
let mut __control_samples=Vec::with_capacity({samples});
for _ in 0..{samples} {{
    let __control_start=std::time::Instant::now();
    for _ in 0..{iterations} {{ std::hint::black_box(__control_operation()); }}
    __control_samples.push(__control_start.elapsed().as_nanos());
}}
println!("CONTROL_BENCH_V1 {{:?}}",__control_samples);
}}
"#,
        setup = input.setup,
        code = input.code,
        warmup = input.iterations.min(100),
        samples = input.samples,
        iterations = input.iterations
    ))
}
pub fn call(args: &Value) -> io::Result<Value> {
    execute(args, |source| crate::toolbox::playground_mode(source, true))
}
fn execute(args: &Value, run: impl FnOnce(&str) -> io::Result<Value>) -> io::Result<Value> {
    let input: Input = serde_json::from_value(args.clone()).map_err(|_| {
        io::Error::other("Expected benchmark code and optional setup, iterations and samples")
    })?;
    let source = source(&input)?;
    let mut result = run(&source)?;
    result["kind"] = json!("rust_microbenchmark");
    result["opt_level"] = json!(3);
    if result["success"] != true {
        return Ok(result);
    }
    let measured = result["output"]
        .as_str()
        .and_then(|s| {
            s.lines()
                .rev()
                .find_map(|line| line.strip_prefix("CONTROL_BENCH_V1 "))
        })
        .ok_or_else(|| {
            io::Error::other("Benchmark did not return timing samples; inspect program output")
        })?;
    let raw: Vec<u64> = serde_json::from_str(measured)
        .map_err(|_| io::Error::other("Invalid benchmark timing samples"))?;
    if raw.len() != input.samples
        || raw.iter().any(|n| *n > 3_000_000_000)
        || result["truncated"] == true
    {
        return Err(io::Error::other(
            "Benchmark timings exceed their declared bounds",
        ));
    }
    let mut per_iteration = raw
        .iter()
        .map(|n| *n as f64 / input.iterations as f64)
        .collect::<Vec<_>>();
    per_iteration.sort_by(f64::total_cmp);
    result["timings"] = json!({"unit":"ns/iteration","iterations":input.iterations,"samples":input.samples,"warmup_iterations":input.iterations.min(100),"sample_elapsed_ns":raw,"min_ns":per_iteration[0],"median_ns":per_iteration[per_iteration.len()/2],"max_ns":per_iteration[per_iteration.len()-1]});
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_before_execution_and_separates_timing_from_diagnostics() {
        for args in [
            json!({"code":""}),
            json!({"code":"1","iterations":0}),
            json!({"code":"1","samples":32}),
            json!({"code":"1","mount":"/"}),
        ] {
            assert!(execute(&args, |_| panic!("Invalid inputs must not execute")).is_err());
        }
        let result=execute(&json!({"code":"std::hint::black_box(7)+1","iterations":10,"samples":3}),|source|{
            assert!(source.contains("cfg(debug_assertions)"));
            assert!(source.contains("Instant::now()"));
            Ok(json!({"success":true,"output":"compiler warning\nCONTROL_BENCH_V1 [20,40,30]\n","image":"pinned-fixture"}))
        }).unwrap();
        assert_eq!(result["timings"]["median_ns"], 3.0);
        assert_eq!(result["image"], "pinned-fixture");
        assert!(execute(&json!({"code":"1","samples":3}), |_| Ok(
            json!({"success":true,"output":"CONTROL_BENCH_V1 [1]"})
        ))
        .is_err());
        let failed = execute(&json!({"code":"bad rust"}), |_| {
            Ok(json!({"success":false,"status":"compile 1"}))
        })
        .unwrap();
        assert!(failed["timings"].is_null());
    }
}
