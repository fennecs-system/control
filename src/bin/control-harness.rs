#[path = "../harness.rs"]
mod harness;

fn main() -> std::io::Result<()> {
    harness::run()
}
use control::config;
