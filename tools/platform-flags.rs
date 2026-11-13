// These std-only container entrypoints are also compiled directly by rustc.
// Keep supported targets explicit: Linux open(2) constants differ by CPU ABI.
#[cfg(target_os = "macos")]
pub const SAFE_READ: i32 = 0x100 | 0x4;
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
pub const SAFE_READ: i32 = 0x8000 | 0x800;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub const SAFE_READ: i32 = 0x20000 | 0x800;
#[cfg(not(any(
    target_os = "macos",
    all(target_os = "linux", target_arch = "aarch64"),
    all(target_os = "linux", target_arch = "x86_64")
)))]
compile_error!("Safe file reads require explicit flags for this operating system and CPU ABI");

#[cfg(test)]
#[path = "platform-flags/tests.rs"]
mod tests;
