//! Names for test scratch resources. Tests run on parallel threads of one
//! process and processes reuse pids, so a name made from the pid or the clock
//! alone can be taken by another test or by a leftover from an earlier run.

/// `<prefix>-<pid>-<stamp>`: unique across the tests of a process and across
/// concurrent test processes. For directories, container names and labels.
pub fn unique(prefix: &str) -> String {
    format!("{prefix}-{}-{:x}", std::process::id(), crate::ids::stamp())
}

/// A not-yet-created directory under the system temporary directory.
pub fn scratch(prefix: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(unique(prefix))
}
