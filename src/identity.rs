use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, path::Path, str::FromStr};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DiscordId(u64);
impl FromStr for DiscordId {
    type Err = &'static str;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() || s.starts_with('0') || !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err("Expected a canonical Discord snowflake ID");
        }
        s.parse::<u64>()
            .map(Self)
            .map_err(|_| "Discord ID exceeds 64 bits")
    }
}
impl fmt::Display for DiscordId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl Serialize for DiscordId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}
impl<'de> Deserialize<'de> for DiscordId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Architect,
    User,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Actor {
    LocalOperator,
    Discord {
        user_id: DiscordId,
        channel_id: DiscordId,
    },
    DiscordDm {
        user_id: DiscordId,
        channel_id: DiscordId,
    },
    #[default]
    Unknown,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    #[default]
    Task,
    Improve,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Workspace {
    User,
    ControlSource,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Principal {
    pub label: String,
    pub user_id: Option<DiscordId>,
    pub role: Role,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub channel_id: Option<DiscordId>,
    // Accept the previous setting on import; it no longer controls node access.
    #[serde(default, rename = "codex_owner_id", skip_serializing)]
    _legacy_owner_id: Option<DiscordId>,
    pub principals: Vec<Principal>,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DmBindings {
    pub recipients: Vec<DmBinding>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DmBinding {
    pub user_id: DiscordId,
    pub channel_id: DiscordId,
}
impl DmBindings {
    pub fn load() -> Result<Self, &'static str> {
        let bytes = crate::config::read_regular(&crate::config::state("discord-dm-bindings.json")?, 1024 * 1024, true)
            .map_err(|_| "DM channel bindings unavailable")?;
        serde_json::from_slice(&bytes).map_err(|_| "Invalid DM channel bindings")
    }
    pub fn contains(&self, user: DiscordId, channel: DiscordId) -> bool {
        self.recipients
            .iter()
            .any(|r| r.user_id == user && r.channel_id == channel)
    }
}
impl Policy {
    pub fn load() -> Result<Self, &'static str> {
        Self::read(&crate::config::directory()?.join("discord-access.json"))
    }
    fn read(path: &Path) -> Result<Self, &'static str> {
        let bytes = crate::config::read_regular(path, 65536, true)
            .map_err(|_| "Discord access policy must be a private regular file (mode 0600, at most 64 KiB)")?;
        let policy: Self =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid Discord access policy")?;
        policy.validate()?;
        Ok(policy)
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.channel_id.is_some() {
            return Err("Server-channel access is disabled. Control is DM-only.");
        }
        let mut ids = std::collections::HashSet::new();
        let mut labels = std::collections::HashSet::new();
        for p in &self.principals {
            if !labels.insert(&p.label) || p.user_id.is_some_and(|id| !ids.insert(id)) {
                return Err("Duplicate principal label or Discord ID");
            }
            if p.label.is_empty()
                || p.label.len() > 64
                || p.label.trim() != p.label
                || p.label.chars().any(char::is_control)
            {
                return Err("Principal labels must contain 1–64 bytes without surrounding whitespace or control characters");
            }
        }
        Ok(())
    }
    pub fn authorize(&self, actor: &Actor, action: Action) -> Result<Workspace, &'static str> {
        let dms = if matches!(actor, Actor::DiscordDm { .. }) {
            DmBindings::load()?
        } else {
            DmBindings::default()
        };
        self.authorize_with_dms(actor, action, &dms)
    }
    pub fn authorize_with_dms(
        &self,
        actor: &Actor,
        action: Action,
        dms: &DmBindings,
    ) -> Result<Workspace, &'static str> {
        let role = match actor {
            Actor::LocalOperator => Role::Architect,
            Actor::Discord { .. } => {
                return Err("Server-channel access is disabled. Control is DM-only.")
            }
            Actor::DiscordDm {
                user_id,
                channel_id,
            } => {
                if !dms.contains(*user_id, *channel_id) {
                    return Err("DM recipient/channel binding does not match");
                }
                self.principals
                    .iter()
                    .find(|p| p.user_id == Some(*user_id))
                    .map(|p| p.role)
                    .ok_or("Discord DM sender is not authorized")?
            }
            Actor::Unknown => {
                return Err("Run has no trusted actor identity; submit a new request")
            }
        };
        match (role, action) {
            (Role::User, Action::Improve) => {
                Err("Only an architect may request Control source changes")
            }
            (_, Action::Task) => Ok(Workspace::User),
            (Role::Architect, Action::Improve) => Ok(Workspace::ControlSource),
        }
    }
    pub fn bind(&mut self, label: &str, id: DiscordId) -> Result<(), &'static str> {
        if self
            .principals
            .iter()
            .any(|p| p.label != label && p.user_id == Some(id))
        {
            return Err("Discord ID is already assigned to another principal");
        }
        let p = self
            .principals
            .iter_mut()
            .find(|p| p.label == label)
            .ok_or("Unknown principal label")?;
        p.user_id = Some(id);
        self.validate()
    }
    pub fn save(&self) -> Result<(), &'static str> {
        self.validate()?;
        let bytes = serde_json::to_vec_pretty(self).map_err(|_| "Could not encode policy")?;
        crate::config::write_private("discord-access.json", &bytes)
            .map_err(|_| "Could not save private Discord policy")
    }
    #[cfg(test)]
    fn save_in(&self, directory: &Path) -> Result<(), &'static str> {
        self.validate()?;
        let bytes = serde_json::to_vec_pretty(self).map_err(|_| "Could not encode policy")?;
        crate::config::write_in(directory, "discord-access.json", &bytes)
            .map_err(|_| "Could not save private Discord policy")
    }
}
pub fn authorize(actor: &Actor, action: Action) -> Result<Workspace, &'static str> {
    if *actor == Actor::LocalOperator {
        return Ok(if action == Action::Improve {
            Workspace::ControlSource
        } else {
            Workspace::User
        });
    }
    Policy::load()?.authorize(actor, action)
}
/// Like `authorize` for many actors, reading the policy and DM bindings at most once.
pub fn authorizer() -> impl FnMut(&Actor, Action) -> Result<Workspace, &'static str> {
    let mut policy = None;
    let mut dms = None;
    move |actor, action| {
        if *actor == Actor::LocalOperator {
            return authorize(actor, action);
        }
        let policy = policy.get_or_insert_with(Policy::load).as_ref().map_err(|e| *e)?;
        let dms = if matches!(actor, Actor::DiscordDm { .. }) {
            dms.get_or_insert_with(DmBindings::load).as_ref().map_err(|e| *e)?
        } else {
            &DmBindings::default()
        };
        policy.authorize_with_dms(actor, action, dms)
    }
}

#[cfg(test)]
mod tests;
