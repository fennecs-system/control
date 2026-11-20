//! Operator opt-ins for automation, in `permissions.json` in the node
//! configuration directory: which request kinds the chat MCP bridge
//! (`control mcp --chat`) may decide, and whether Control's own agents may use
//! the restricted chat tools. Everything is off by default; Manage →
//! Permissions edits it. See docs/CHAT-MCP.md.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Approvals {
    /// Folder access (`/open`): host paths become readable by the task.
    pub folder: bool,
    /// Network ports for persistent services.
    pub port: bool,
    /// Persistent bot requests.
    pub bot: bool,
    /// Design questions (answering steers a paused task).
    pub question: bool,
}
impl Approvals {
    pub const KINDS: [&'static str; 4] = ["folder", "port", "bot", "question"];
    pub fn allows(&self, kind: &str) -> bool {
        match kind {
            "folder" => self.folder,
            "port" => self.port,
            "bot" => self.bot,
            "question" => self.question,
            _ => false,
        }
    }
    pub fn any(&self) -> bool {
        self.folder || self.port || self.bot || self.question
    }
    pub fn set(&mut self, kind: &str, allowed: bool) {
        match kind {
            "folder" => self.folder = allowed,
            "port" => self.port = allowed,
            "bot" => self.bot = allowed,
            "question" => self.question = allowed,
            _ => {}
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Permissions {
    /// Request kinds `control mcp --chat` may approve or deny.
    pub mcp_approvals: Approvals,
    /// Control's agents may drive DMs inside their own task (`control_chat`).
    pub agent_chat_tools: bool,
}

const FILE: &str = "permissions.json";

/// The node's permissions; a missing or unreadable file means all off.
pub fn read() -> Permissions {
    let read = || {
        let path = crate::config::directory().ok()?.join(FILE);
        let bytes = crate::config::read_regular(&path, 16384, false).ok()?;
        serde_json::from_slice(&bytes).ok()
    };
    read().unwrap_or_default()
}
pub fn save(permissions: &Permissions) -> Result<(), &'static str> {
    let bytes = serde_json::to_vec_pretty(permissions).map_err(|_| "Could not encode permissions.")?;
    crate::config::write_private(FILE, &bytes).map_err(|_| "Could not save permissions.")
}
