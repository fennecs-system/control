//! The container engine binary, shared by Control's engine module and
//! standalone tools so the CLI is named in exactly one place.
pub fn command() -> std::process::Command {
    std::process::Command::new("docker")
}
