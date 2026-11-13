use serde_json::{Value, json};
use std::{collections::BTreeMap, env, fs, path::Path, process::Command, time::Instant};
use syn::{spanned::Spanned, visit::Visit};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn run(program: &str, args: &[&str], source: &Path) -> Result<Vec<u8>> {
    let output = Command::new(program)
        .args(args)
        .env("RAYON_NUM_THREADS", "2")
        .current_dir(source)
        .output()?;
    if !output.status.success() {
        return Err(format!("{program}: {}", String::from_utf8_lossy(&output.stderr)).into());
    }
    Ok(output.stdout)
}

fn read(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn check_snapshot(path: &Path) -> Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(format!("snapshot contains symlink: {}", entry.path().display()).into());
        }
        if kind.is_dir() {
            if matches!(
                entry.file_name().to_str(),
                Some(".git" | "target" | "dist" | "node_modules")
            ) {
                return Err(
                    "use a git archive snapshot, without generated/build directories".into(),
                );
            }
            check_snapshot(&entry.path())?;
        }
    }
    Ok(())
}

fn group(name: &str) -> String {
    let name = name.trim_start_matches("./");
    match name {
        "src/terminal.rs" | "src/bin/control-harness.rs" => "terminal",
        "src/client.rs" => "shared-client",
        _ if name.starts_with("crates/") || name.starts_with("tools/") => {
            return name.split('/').take(2).collect::<Vec<_>>().join("/");
        }
        _ => name.split('/').next().unwrap_or("root"),
    }
    .into()
}

#[derive(Default)]
struct TestScopes(Vec<(usize, usize)>);

impl<'ast> Visit<'ast> for TestScopes {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        let attrs = match item {
            syn::Item::Mod(x) => &x.attrs,
            syn::Item::Fn(x) => &x.attrs,
            syn::Item::Impl(x) => &x.attrs,
            _ => {
                syn::visit::visit_item(self, item);
                return;
            }
        };
        if attrs.iter().any(|a| {
            a.path().segments.last().is_some_and(|s| s.ident == "test")
                || (a.path().is_ident("cfg")
                    && a.parse_args::<syn::Ident>().is_ok_and(|i| i == "test"))
        }) {
            self.0
                .push((item.span().start().line, item.span().end().line));
        }
        syn::visit::visit_item(self, item);
    }
}

fn functions(space: &Value, file: &str, tests: &[(usize, usize)], output: &mut Vec<Value>) {
    let name = space["name"].as_str().unwrap_or("");
    let line = space["start_line"].as_u64().unwrap_or(0) as usize;
    let test = file.split('/').any(|part| part == "tests")
        || Path::new(file)
            .file_name()
            .is_some_and(|name| name == "tests.rs")
        || tests
            .iter()
            .any(|&(start, end)| (start..=end).contains(&line));
    if space["kind"] == "function" && name != "<anonymous>" {
        output.push(json!({
            "file": file.trim_start_matches("./"), "name": name,
            "line": space["start_line"], "test_scope": test,
            "cyclomatic": space["metrics"]["cyclomatic"]["sum"],
            "cognitive": space["metrics"]["cognitive"]["sum"],
            "physical_lines": space["metrics"]["loc"]["sloc"],
        }));
    }
    if let Some(children) = space["spaces"].as_array() {
        for child in children {
            functions(child, file, tests, output);
        }
    }
}

fn collect(path: &Path, source: &Path, output: &mut Vec<Value>) -> Result<usize> {
    let mut files = 0;
    for entry in fs::read_dir(path)? {
        let path = entry?.path();
        if path.is_dir() {
            files += collect(&path, source, output)?;
        } else if path.extension().is_some_and(|ext| ext == "json") {
            let space = read(&path)?;
            let file = space["name"].as_str().ok_or("missing analyzer filename")?;
            let file = Path::new(file)
                .strip_prefix(source)?
                .to_str()
                .ok_or("non-UTF8 source filename")?;
            let tree = syn::parse_file(&fs::read_to_string(source.join(file))?)?;
            let mut tests = TestScopes::default();
            tests.visit_file(&tree);
            functions(&space, file, &tests.0, output);
            files += 1;
        }
    }
    Ok(files)
}

fn scan(source: &Path, output: &Path) -> Result<()> {
    let started = Instant::now();
    if output.starts_with(source) || fs::read_dir(output)?.next().is_some() {
        return Err("output must be empty and outside the source snapshot".into());
    }
    check_snapshot(source)?;
    fs::create_dir_all(output.join("complexity"))?;
    fs::write(
        output.join("loc.json"),
        run(
            "tokei",
            &["--hidden", "--no-ignore", "--output", "json", "."],
            source,
        )?,
    )?;
    run(
        "rust-code-analysis-cli",
        &[
            "-m",
            "-p",
            source.to_str().ok_or("source path is not UTF-8")?,
            "-I",
            "**/*.rs",
            "-j",
            "2",
            "-O",
            "json",
            "-o",
            output
                .join("complexity")
                .to_str()
                .ok_or("output path is not UTF-8")?,
        ],
        source,
    )?;
    // No project config, model downloads, semantic API calls or clone failure gate.
    fs::write(output.join("clone-config.json"), "{}")?;
    run(
        "jscpd",
        &[
            "--config",
            output.join("clone-config.json").to_str().unwrap(),
            "--format",
            "rust",
            "--mode",
            "weak",
            "--min-lines",
            "12",
            "--min-tokens",
            "100",
            "--no-gitignore",
            "--reporters",
            "json",
            "--output",
            output.to_str().unwrap(),
            ".",
        ],
        source,
    )?;
    let loc = read(&output.join("loc.json"))?;
    let rust = loc["Rust"]["reports"]
        .as_array()
        .ok_or("no Rust files counted")?;
    let mut groups = BTreeMap::<String, u64>::new();
    for file in rust {
        *groups
            .entry(group(file["name"].as_str().ok_or("missing LOC filename")?))
            .or_default() += file["stats"]["code"].as_u64().ok_or("missing LOC count")?;
    }
    let mut items = Vec::new();
    let analyzed = collect(&output.join("complexity"), source, &mut items)?;
    if analyzed != rust.len() {
        return Err(format!(
            "complexity coverage mismatch: {analyzed} analyzed, {} Rust files counted",
            rust.len()
        )
        .into());
    }
    items.sort_by(|a, b| {
        b["cyclomatic"]
            .as_f64()
            .unwrap_or(0.0)
            .total_cmp(&a["cyclomatic"].as_f64().unwrap_or(0.0))
            .then_with(|| a["file"].as_str().cmp(&b["file"].as_str()))
            .then_with(|| a["line"].as_u64().cmp(&b["line"].as_u64()))
    });
    if items.is_empty() {
        return Err("no functions analyzed".into());
    }
    let clones = read(&output.join("jscpd-report.json"))?;
    let mut versions = BTreeMap::new();
    for program in ["tokei", "rust-code-analysis-cli", "jscpd"] {
        versions.insert(
            program,
            String::from_utf8(run(program, &["--version"], source)?)?
                .trim()
                .to_string(),
        );
    }
    let summary = json!({"schema": 1, "versions": versions, "elapsed_seconds": started.elapsed().as_secs_f64(),
        "rust_code_lines": loc["Rust"]["code"], "rust_files": rust.len(), "groups": groups,
        "named_functions": items.len(), "test_scope_functions": items.iter().filter(|f| f["test_scope"] == true).count(),
        "clones": clones["statistics"]["total"], "functions": items});
    fs::write(
        output.join("summary.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    Ok(())
}

fn compare(before: &Value, after: &Value) -> Result<()> {
    if before["schema"] != after["schema"] || before["versions"] != after["versions"] {
        return Err(
            "metrics schema/tool versions differ; rescan both revisions with the same tools".into(),
        );
    }
    println!(
        "## Codebase metrics\n\nAdvisory observations, not quality scores or merge thresholds. Rust LOC includes inline tests.\n"
    );
    println!("| Measure | Base | Head | Change |\n|---|---:|---:|---:|");
    for (name, key) in [
        ("Rust code lines", "rust_code_lines"),
        ("Rust files", "rust_files"),
        ("Named functions", "named_functions"),
        ("Test-scope functions", "test_scope_functions"),
    ] {
        let a = before[key].as_i64().ok_or("missing base count")?;
        let b = after[key].as_i64().ok_or("missing head count")?;
        println!("| {name} | {a} | {b} | {:+} |", b - a);
    }
    println!(
        "| Rust clone pairs (≥12 lines, ≥100 tokens) | {} | {} | |",
        before["clones"]["clones"], after["clones"]["clones"]
    );
    println!(
        "\nRust code lines by boundary (inline tests included):\n\n| Boundary | Base | Head |\n|---|---:|---:|"
    );
    let mut groups = BTreeMap::new();
    for report in [before, after] {
        for name in report["groups"].as_object().ok_or("missing groups")?.keys() {
            groups.insert(name, ());
        }
    }
    for name in groups.keys() {
        println!(
            "| {name} | {} | {} |",
            before["groups"][*name].as_u64().unwrap_or(0),
            after["groups"][*name].as_u64().unwrap_or(0)
        );
    }
    println!(
        "\nHead complexity hotspots outside recognized test scopes:\n\n| Function | Cyclomatic | Cognitive | Physical lines |\n|---|---:|---:|---:|"
    );
    for f in after["functions"]
        .as_array()
        .ok_or("missing functions")?
        .iter()
        .filter(|f| f["test_scope"] != true)
        .take(15)
    {
        // Filenames/function names come from source, so escape Markdown separators.
        let label = format!(
            "{}:{} {}",
            f["file"].as_str().unwrap_or("?"),
            f["line"],
            f["name"].as_str().unwrap_or("?")
        )
        .replace('|', "\\|")
        .replace(['\n', '\r'], " ");
        println!(
            "| {label} | {} | {} | {} |",
            f["cyclomatic"], f["cognitive"], f["physical_lines"]
        );
    }
    println!(
        "\nRaw artifacts retain all function scopes, LOC by language/file and clone locations. Inline test classification is approximate; macros are not expanded. Halstead metrics are available in raw complexity JSON, not a quality/entropy score.\n"
    );
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<_> = env::args().collect();
    match args.as_slice() {
        [_, mode, source, output] if mode == "scan" => scan(&fs::canonicalize(source)?, &fs::canonicalize(output)?),
        [_, mode, before, after] if mode == "compare" => compare(&read(Path::new(before))?, &read(Path::new(after))?),
        _ => Err("usage: control-code-metrics scan SNAPSHOT EXISTING_OUTPUT_DIR | compare BASE_SUMMARY HEAD_SUMMARY".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_preserve_presentation_boundary() {
        assert_eq!(group("./src/terminal.rs"), "terminal");
        assert_eq!(group("web/src/main.rs"), "web");
        assert_eq!(group("crates/bot/src/lib.rs"), "crates/bot");
    }

    #[test]
    fn functions_use_test_attribute_spans_and_skip_anonymous_closures() {
        let source =
            syn::parse_file("#[cfg(test)] mod checks {\n fn scenario() {}\n}\nfn live() {}\n")
                .unwrap();
        let mut tests = TestScopes::default();
        tests.visit_file(&source);
        let tree = json!({"spaces": [{"name": "scenario", "kind": "function", "start_line": 2, "spaces": [{"name": "<anonymous>", "kind": "function"}]}, {"name":"live", "kind":"function", "start_line":4}]});
        let mut output = vec![];
        functions(&tree, "src/example.rs", &tests.0, &mut output);
        assert_eq!(output.len(), 2);
        assert_eq!(output[0]["test_scope"], true);
        assert_eq!(output[1]["test_scope"], false);
    }

    #[test]
    fn external_test_modules_include_unannotated_helpers() {
        let helper = json!({"name": "fixture", "kind": "function", "start_line": 1});
        for (file, expected) in [
            ("src/application/notifications/tests.rs", true),
            ("src/room_wakes/tests/helpers.rs", true),
            ("crates/bot/tests.rs", true),
            ("src/application/notifications.rs", false),
            ("src/latest_tests.rs", false),
        ] {
            let mut output = vec![];
            functions(&helper, file, &[], &mut output);
            assert_eq!(output[0]["test_scope"], expected, "{file}");
        }
    }
}
