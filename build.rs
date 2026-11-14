use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=CONTROL_BUILD_REVISION");
    let revision = env::var("CONTROL_BUILD_REVISION").unwrap_or_default();
    let (sha, dirty) = revision.strip_suffix("-dirty").map_or((revision.as_str(), false), |sha| (sha, true));
    assert!(revision.is_empty() || (sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit())), "CONTROL_BUILD_REVISION must be a full Git SHA, optionally followed by -dirty");
    println!("cargo:rustc-env=CONTROL_BUILD_REVISION={}", sha.to_ascii_lowercase());
    println!("cargo:rustc-env=CONTROL_BUILD_DIRTY={dirty}");
    println!("cargo:rerun-if-changed=web/dist");
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let mut files: Vec<_> = fs::read_dir(root.join("web/dist"))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && matches!(
                    path.extension().and_then(|s| s.to_str()),
                    Some("html" | "js" | "wasm" | "css")
                )
        })
        .collect();
    files.sort();
    if env::var("PROFILE").as_deref() == Ok("release")
        && !files
            .iter()
            .any(|p| p.extension().is_some_and(|s| s == "wasm"))
    {
        panic!("Build the Rust/WASM UI first: cd web && trunk build --release --locked");
    }
    let mut source = String::from("pub const ASSETS: &[(&str, &[u8])] = &[\n");
    for path in files {
        let name = path
            .file_name()
            .unwrap()
            .to_str()
            .expect("UTF-8 asset name");
        source.push_str(&format!(
            "({name:?}, include_bytes!({:?})),\n",
            path.to_str().unwrap()
        ));
    }
    source.push_str("];\n");
    fs::write(
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("web_assets.rs"),
        source,
    )
    .unwrap();
}
