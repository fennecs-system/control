//! Only `src/store.rs` may name the mutex around the application state, so
//! what sits behind the `Store` handle can change in that one module.
use std::path::{Path, PathBuf};

/// The module that owns the lock.
const OWNER: &str = "src/store.rs";

/// Whether `text` names `Mutex<State>` or a `MutexGuard` over `State`, in any
/// spelling rustfmt allows (`crate::State`, a lifetime, spaces).
fn names_raw_state(text: &str) -> bool {
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    ["Mutex<", "MutexGuard<"].iter().any(|open| {
        compact.match_indices(open).any(|(at, _)| {
            let inside = &compact[at + open.len()..];
            let inside = &inside[..inside.find('>').unwrap_or(inside.len())];
            let last = inside.rsplit(',').next().unwrap_or(inside);
            matches!(last, "State" | "crate::State" | "super::State")
        })
    })
}

/// Every Rust source under `dir`, depth first.
fn sources(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .expect("source directory is readable")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .flat_map(|path| {
            if path.is_dir() {
                sources(&path)
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                vec![path]
            } else {
                Vec::new()
            }
        })
        .collect()
}

#[test]
fn only_the_store_module_names_the_state_mutex() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let offenders = sources(&root.join("src"))
        .into_iter()
        .filter(|path| {
            path.strip_prefix(root)
                .is_ok_and(|relative| relative != Path::new(OWNER))
        })
        .filter(|path| names_raw_state(&std::fs::read_to_string(path).expect("source is readable")))
        .collect::<Vec<_>>();
    assert_eq!(offenders, Vec::<PathBuf>::new());
    assert!(names_raw_state(
        &std::fs::read_to_string(root.join(OWNER)).expect("store module is readable")
    ));
}

#[test]
fn the_rule_finds_each_spelling() {
    [
        "fn f(shared: &Arc<Mutex<State>>) {}",
        "fn f(shared: &std::sync::Mutex<crate::State>) {}",
        "fn f(state: MutexGuard<'_, State>) {}",
        "struct S<'a>(MutexGuard<'a, crate::State>);",
        "let s: Arc<Mutex< State >> = todo!();",
    ]
    .iter()
    .for_each(|text| assert!(names_raw_state(text), "{text}"));
    [
        "fn f(shared: &Store) {}",
        "static SLOT: Mutex<Option<Child>> = Mutex::new(None);",
        "fn f(state: MutexGuard<'_, StateFile>) {}",
        "let held: Mutex<Vec<State>> = todo!();",
    ]
    .iter()
    .for_each(|text| assert!(!names_raw_state(text), "{text}"));
}
