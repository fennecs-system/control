use std::{env, fs, path::PathBuf};

fn main() {
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
