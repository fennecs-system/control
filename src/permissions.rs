//! Operator opt-ins for automation, in `permissions.json` in the node
//! configuration directory: which request kinds the chat MCP bridge
//! (`control mcp --chat`) may decide, whether Control's own agents may use
//! the restricted chat tools, and the automatic approval policy for routine
//! agent requests. Everything is off (strict) by default; Manage → Permissions
//! edits it. See docs/CHAT-MCP.md and docs/PORTS.md (automatic approval).
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
    /// Shared-knowledge proposals (accept or reject, as in Knowledge → Review).
    pub knowledge: bool,
}
impl Approvals {
    pub const KINDS: [&'static str; 5] = ["folder", "port", "bot", "question", "knowledge"];
    pub fn allows(&self, kind: &str) -> bool {
        match kind {
            "folder" => self.folder,
            "port" => self.port,
            "bot" => self.bot,
            "question" => self.question,
            "knowledge" => self.knowledge,
            _ => false,
        }
    }
    pub fn any(&self) -> bool {
        self.folder || self.port || self.bot || self.question || self.knowledge
    }
    pub fn set(&mut self, kind: &str, allowed: bool) {
        match kind {
            "folder" => self.folder = allowed,
            "port" => self.port = allowed,
            "bot" => self.bot = allowed,
            "question" => self.question = allowed,
            "knowledge" => self.knowledge = allowed,
            _ => {}
        }
    }
}

/// The node-wide switch. `Strict` is today's behaviour: every request waits
/// for a person.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Strict,
    Auto,
}
/// Per request kind, under `Mode::Auto`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KindMode {
    Ask,
    #[default]
    Auto,
}

/// At most this many standing targets and strict principals.
pub const MAX_STANDING: usize = 32;
pub const MAX_STRICT: usize = 64;

/// Automatic approval of routine requests. Only ports and persistent bots
/// have a mode; folder access (host files) and anything credential-related
/// always ask and have no setting here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AutoApproval {
    pub mode: Mode,
    pub port: KindMode,
    pub bot: KindMode,
    /// Exact `host:port` egress targets the operator has approved standing,
    /// for example a local testbed at `127.0.0.1:25565`. The only way a
    /// loopback target is approved automatically.
    pub standing_targets: Vec<String>,
    /// Principals whose tasks always ask, even in `Mode::Auto`:
    /// `local` (this host's operator) or `discord:USER_ID`.
    pub strict: Vec<String>,
    /// Forward an FYI notice (no action needed) for each automatic decision,
    /// rate-limited, through the operator channels.
    pub notices: bool,
}
impl Default for AutoApproval {
    fn default() -> Self {
        Self { mode: Mode::Strict, port: KindMode::Auto, bot: KindMode::Auto, standing_targets: Vec::new(), strict: Vec::new(), notices: true }
    }
}
impl AutoApproval {
    /// Whether `target` (`host:port`) is one of the standing targets.
    pub fn standing(&self, host: &str, port: u16) -> bool {
        self.standing_targets.iter().any(|entry| split_target(entry).is_some_and(|(h, p)| p == port && h.eq_ignore_ascii_case(host)))
    }
    /// Checks the operator's entries; the page refuses to save invalid ones.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.standing_targets.len() > MAX_STANDING || self.strict.len() > MAX_STRICT {
            return Err("Too many standing targets or strict principals.");
        }
        if self.standing_targets.iter().any(|entry| split_target(entry).is_none()) {
            return Err("Standing targets are host:port, one per line (for example 127.0.0.1:25565).");
        }
        if !self.strict.iter().all(|entry| entry == "local" || entry.strip_prefix("discord:").is_some_and(|id| !id.is_empty() && id.len() <= 20 && id.bytes().all(|b| b.is_ascii_digit()))) {
            return Err("Strict principals are local or discord:USER_ID.");
        }
        Ok(())
    }
}
/// `host:port` or `[v6]:port` with a hostname or literal IP host.
fn split_target(entry: &str) -> Option<(&str, u16)> {
    let (host, port) = entry.rsplit_once(':')?;
    let host = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
    let port = port.parse::<u16>().ok().filter(|p| *p != 0)?;
    let valid = !host.is_empty() && host.len() <= 253
        && (host.parse::<std::net::IpAddr>().is_ok() || host.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-')));
    valid.then_some((host, port))
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Permissions {
    /// Request kinds `control mcp --chat` may approve or deny.
    pub mcp_approvals: Approvals,
    /// Control's agents may drive DMs inside their own task (`control_chat`).
    pub agent_chat_tools: bool,
    /// Automatic approval of routine port and bot requests.
    pub auto_approval: AutoApproval,
}

const FILE: &str = "permissions.json";

/// The node's permissions; a missing or unreadable file means all off.
pub fn read() -> Permissions {
    let read = || {
        let path = control_config::directory().ok()?.join(FILE);
        let bytes = control_config::read_regular(&path, 65536, false).ok()?;
        serde_json::from_slice::<Permissions>(&bytes).ok().filter(|p| p.auto_approval.validate().is_ok())
    };
    read().unwrap_or_default()
}
pub fn save(permissions: &Permissions) -> Result<(), &'static str> {
    permissions.auto_approval.validate()?;
    let bytes = serde_json::to_vec_pretty(permissions).map_err(|_| "Could not encode permissions.")?;
    control_config::write_private(FILE, &bytes).map_err(|_| "Could not save permissions.")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn auto_approval_defaults_to_strict_and_older_files_still_read() {
        let old: Permissions = serde_json::from_str(r#"{"mcp_approvals":{"port":true},"agent_chat_tools":false}"#).unwrap();
        assert_eq!(old.auto_approval.mode, Mode::Strict);
        assert!(old.auto_approval.notices && old.auto_approval.standing_targets.is_empty());
        let policy = AutoApproval { standing_targets: vec!["127.0.0.1:25565".into(), "[::1]:7000".into(), "Example.com:8443".into()], ..Default::default() };
        assert!(policy.validate().is_ok());
        assert!(policy.standing("127.0.0.1", 25565) && policy.standing("::1", 7000) && policy.standing("example.COM", 8443));
        assert!(!policy.standing("127.0.0.1", 25566));
        for bad in ["127.0.0.1", "host:0", "a b:80", ":80"] {
            assert!(AutoApproval { standing_targets: vec![bad.into()], ..Default::default() }.validate().is_err(), "{bad}");
        }
        assert!(AutoApproval { strict: vec!["discord:12a".into()], ..Default::default() }.validate().is_err());
        assert!(AutoApproval { strict: vec!["local".into(), "discord:123".into()], ..Default::default() }.validate().is_ok());
    }
}
