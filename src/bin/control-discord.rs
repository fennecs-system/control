#[path = "../discord.rs"]
mod discord;
#[path = "../discord_bridge.rs"]
mod discord_bridge;
#[path = "../discord_permissions.rs"]
mod discord_permissions;
#[path = "../identity.rs"]
mod identity;
use control::config;

fn main() {
    discord_bridge::command(&std::env::args().collect::<Vec<_>>());
}
