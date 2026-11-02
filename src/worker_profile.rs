//! Captured capability of an operator-pinned image, never a model Docker option.
use serde::{Deserialize, Serialize};
use std::io;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    #[default]
    Lite,
    Full,
}

impl Profile {
    pub fn is_lite(&self) -> bool {
        *self == Self::Lite
    }

    /// Only credential-free tool containers may use Full's private writable layer.
    pub fn readonly_root(self) -> bool {
        self == Self::Lite
    }

    pub fn service_memory(self) -> u64 {
        match self {
            Self::Lite => 512 * 1024 * 1024,
            Self::Full => 2 * 1024 * 1024 * 1024,
        }
    }

    pub fn service_pids(self) -> u64 {
        match self {
            Self::Lite => 64,
            Self::Full => 128,
        }
    }

    pub fn service_workspace_mb(self) -> u64 {
        match self {
            Self::Lite => 64,
            Self::Full => 512,
        }
    }

    pub fn inspect(image: &str) -> io::Result<Self> {
        if !crate::runtime::pinned_image(image) {
            return Err(io::Error::other("Worker image must be pinned by digest"));
        }
        let marker = crate::engine::query(&[
            "image",
            "inspect",
            "--format",
            "{{index .Config.Labels \"dev.control.worker-profile\"}}",
            image,
        ])?;
        Self::from_marker(marker.trim())
    }

    fn from_marker(marker: &str) -> io::Result<Self> {
        match marker {
            "" | "<no value>" | "lite" => Ok(Self::Lite),
            "full-v1" => Ok(Self::Full),
            _ => Err(io::Error::other(
                "Unsupported worker profile; rebuild the pinned image",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_and_unknown_images_fail_closed() {
        for marker in ["", "<no value>", "lite"] {
            assert!(Profile::from_marker(marker).unwrap().readonly_root());
        }
        assert!(!Profile::from_marker("full-v1").unwrap().readonly_root());
        assert!(Profile::from_marker("full-v2").is_err());
    }

    #[test]
    #[ignore = "requires CONTROL_QA_FULL_IMAGE pinned full worker and Linux Docker; no credentials/models"]
    fn live_private_nix_build_and_runtime_isolation() {
        let image = std::env::var("CONTROL_QA_FULL_IMAGE").unwrap();
        assert_eq!(Profile::inspect(&image).unwrap(), Profile::Full);
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };
        assert_ne!(uid, 0);
        let run = |script: &str| {
            let result = crate::engine::cli()
                .args([
                    "run",
                    "--rm",
                    "--pull=never",
                    "--network=none",
                    "--cap-drop=ALL",
                    "--security-opt=no-new-privileges",
                    "--memory=2g",
                    "--cpus=2",
                    "--pids-limit=256",
                    "--user",
                    &format!("{uid}:{gid}"),
                    "--tmpfs",
                    &format!("/home/agent:rw,nosuid,nodev,uid={uid},gid={gid},size=64m"),
                    "--tmpfs",
                    "/tmp:rw,nosuid,nodev,size=128m",
                    &image,
                    "bash",
                    "-c",
                    script,
                ])
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            String::from_utf8(result.stdout).unwrap()
        };
        let script = r#"set -eu
node --version >&2; npm --version >&2; java -version >&2; javac --version >&2; python3 --version >&2; curl --version >&2; nix --version >&2
test ! -w /nix; test ! -w /bin
helper=$(readlink -f /bin/control-tcp-relay)
test ! -w "$helper"
store_path=${helper%/bin/control-tcp-relay}
if mv "$store_path" "$store_path.replaced" 2>/dev/null; then echo trusted-store-path-replaceable >&2; exit 41; fi
cat > /tmp/check.nix <<'EOF'
let pkgs = import <nixpkgs> {}; in builtins.derivation {
 name = "control-private-nix-check";
 system = builtins.currentSystem;
 builder = "${pkgs.bash}/bin/bash";
 args = [ "-c" "${pkgs.coreutils}/bin/mkdir -p $out; ${pkgs.coreutils}/bin/printf private-nix-build > $out/result" ];
}
EOF
out=$(nix-build --no-out-link --option substituters '' /tmp/check.nix)
test "$(cat "$out/result")" = private-nix-build
nix-store --check-validity "$out"
cached_node=$(nix-build --no-out-link --option substituters '' '<nixpkgs>' -A nodejs)
"$cached_node/bin/node" --version >&2
printf '%s\n' "$out"
"#;
        let output = run(script);
        let path = output.trim();
        assert!(path.starts_with("/nix/store/") && path.ends_with("-control-private-nix-check"));
        assert!(
            path.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/-".contains(&b))
        );
        run(&format!(
            "set -eu; test ! -e '{path}'; if nix-store --check-validity '{path}' 2>/dev/null; then exit 42; fi; printf isolated"
        ));
        let workspace =
            std::env::temp_dir().join(format!("control-profile-qa-{}", std::process::id()));
        std::fs::create_dir(&workspace).unwrap();
        let actual = crate::claude::shell_scoped(
            &image,
            &format!("control-profile-qa-{}", std::process::id()),
            "synthetic-profile-qa",
            &workspace,
            script,
            false,
        )
        .unwrap();
        std::fs::remove_dir(&workspace).unwrap();
        assert_eq!(actual["exit_code"], 0, "{actual}");
        assert_eq!(actual["stdout"].as_str().unwrap().trim(), path);
        println!(
            "Local Nix derivation built and registered; trusted paths protected; second container store/database isolated."
        );
    }
}
