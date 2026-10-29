use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, fs, io::Write, path::Path, str::FromStr};

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
    #[serde(default)]
    pub codex_owner_id: Option<DiscordId>,
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
        let bytes = fs::read(".control/discord-dm-bindings.json")
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
        Self::read(Path::new(".control/discord-access.json"))
    }
    fn read(path: &Path) -> Result<Self, &'static str> {
        use std::os::unix::fs::PermissionsExt;
        let metadata =
            fs::symlink_metadata(path).map_err(|_| "Discord access policy unavailable")?;
        if !metadata.is_file()
            || metadata.permissions().mode() & 0o077 != 0
            || metadata.len() > 65536
        {
            return Err(
                "Discord access policy must be a private regular file (mode 0600, at most 64 KiB)",
            );
        }
        let bytes = fs::read(path).map_err(|_| "Discord access policy unavailable")?;
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
        if self
            .codex_owner_id
            .is_some_and(|owner| !ids.contains(&owner))
        {
            return Err("Codex credential owner must be a bound principal");
        }
        Ok(())
    }
    pub fn authorize_codex_with_dms(
        &self,
        actor: &Actor,
        action: Action,
        dms: &DmBindings,
    ) -> Result<Workspace, &'static str> {
        self.validate()?;
        let workspace = self.authorize_with_dms(actor, action, dms)?;
        match actor {
            Actor::LocalOperator => Ok(workspace),
            Actor::DiscordDm { user_id, .. } if Some(*user_id) == self.codex_owner_id => {
                Ok(workspace)
            }
            _ => Err(
                "This Codex login is reserved for its owner. Use your own Control node and login.",
            ),
        }
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
        self.save_in(Path::new(".control"))
    }
    fn save_in(&self, directory: &Path) -> Result<(), &'static str> {
        self.validate()?;
        let bytes = serde_json::to_vec_pretty(self).map_err(|_| "Could not encode policy")?;
        let temporary = directory.join(format!("discord-access.{}.tmp", std::process::id()));
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|_| "Could not create private policy checkpoint")?;
        let result = (|| -> std::io::Result<()> {
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temporary, directory.join("discord-access.json"))?;
            fs::File::open(directory)?.sync_all()
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result.map_err(|_| "Could not save private Discord policy")
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

// Subscription ownership is independent of role authority. Reuse this boundary
// at admission, retry, dispatch and worker-tool invocation.
pub fn authorize_codex(actor: &Actor, action: Action) -> Result<Workspace, &'static str> {
    if *actor == Actor::LocalOperator {
        return authorize(actor, action);
    }
    let policy = Policy::load()?;
    let dms = if matches!(actor, Actor::DiscordDm { .. }) {
        DmBindings::load()?
    } else {
        DmBindings::default()
    };
    policy.authorize_codex_with_dms(actor, action, &dms)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy() -> Policy {
        serde_json::from_str(r#"{"channel_id":null,"principals":[{"label":"geodesic","user_id":"1","role":"architect"},{"label":"vivkelly","user_id":"2","role":"user"},{"label":"olliev","user_id":"3","role":"user"}]}"#).unwrap()
    }
    fn actor(id: &str) -> Actor {
        Actor::DiscordDm {
            user_id: id.parse().unwrap(),
            channel_id: "100".parse().unwrap(),
        }
    }
    #[test]
    fn ids_are_canonical_and_lossless() {
        for bad in [
            "geodesic",
            "@geodesic",
            "0",
            "01",
            "-1",
            "18446744073709551616",
        ] {
            assert!(bad.parse::<DiscordId>().is_err());
        }
        let id: DiscordId = "18446744073709551615".parse().unwrap();
        assert_eq!(
            serde_json::to_string(&id).unwrap(),
            "\"18446744073709551615\""
        );
    }
    #[test]
    fn subscription_ownership_is_explicit_and_independent_of_roles() {
        let mut p = policy();
        let dms = DmBindings {
            recipients: ["1", "2", "3"]
                .iter()
                .map(|id| DmBinding {
                    user_id: id.parse().unwrap(),
                    channel_id: "100".parse().unwrap(),
                })
                .collect(),
        };
        // Existing policies have no implicit subscription owner, even an architect.
        assert!(p
            .authorize_codex_with_dms(&actor("1"), Action::Task, &dms)
            .is_err());
        p.codex_owner_id = Some("2".parse().unwrap());
        assert_eq!(
            p.authorize_codex_with_dms(&actor("2"), Action::Task, &dms),
            Ok(Workspace::User)
        );
        assert!(p
            .authorize_codex_with_dms(&actor("2"), Action::Improve, &dms)
            .is_err());
        assert!(p
            .authorize_codex_with_dms(&actor("1"), Action::Task, &dms)
            .is_err());
        assert!(p
            .authorize_codex_with_dms(&actor("3"), Action::Task, &dms)
            .is_err());
        assert!(p
            .authorize_codex_with_dms(&actor("2"), Action::Task, &DmBindings::default())
            .is_err());
        p.principals[1].role = Role::Architect;
        p.principals[1].label = "another-operator".into();
        assert_eq!(
            p.authorize_codex_with_dms(&actor("2"), Action::Improve, &dms),
            Ok(Workspace::ControlSource)
        );
        // A role or label change cannot transfer ownership to another ID.
        assert!(p
            .authorize_codex_with_dms(&actor("1"), Action::Improve, &dms)
            .is_err());
        p.codex_owner_id = None;
        assert!(p
            .authorize_codex_with_dms(&actor("2"), Action::Task, &dms)
            .is_err());
        p.codex_owner_id = Some("4".parse().unwrap());
        assert!(p.validate().is_err());
    }

    #[test]
    fn arbitrary_principals_validate_but_ambiguous_labels_do_not() {
        let mut p: Policy = serde_json::from_str(r#"{"channel_id":null,"principals":[{"label":"my operator","user_id":"9","role":"architect"},{"label":"friend","user_id":null,"role":"user"}]}"#).unwrap();
        assert!(p.validate().is_ok());
        for label in ["", " leading", "trailing ", "line\nbreak"] {
            p.principals[0].label = label.into();
            assert!(p.validate().is_err());
        }
    }

    #[test]
    fn policy_checkpoints_are_private_and_do_not_overwrite_unknown_temporary_files() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let directory = std::env::temp_dir().join(format!(
            "control-policy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&directory).unwrap();
        let mut p = policy();
        p.codex_owner_id = Some("1".parse().unwrap());
        p.save_in(&directory).unwrap();
        let path = directory.join("discord-access.json");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            Policy::read(&path).unwrap().codex_owner_id,
            p.codex_owner_id
        );
        let temporary = directory.join(format!("discord-access.{}.tmp", std::process::id()));
        fs::write(&temporary, b"preserve evidence").unwrap();
        p.codex_owner_id = None;
        assert!(p.save_in(&directory).is_err());
        assert_eq!(fs::read(&temporary).unwrap(), b"preserve evidence");
        assert!(Policy::read(&path).unwrap().codex_owner_id.is_some());
        let linked = directory.join("linked.json");
        symlink(&path, &linked).unwrap();
        assert!(Policy::read(&linked).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(Policy::read(&path).is_err());
        fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn roles_bound_actions_and_workspaces() {
        let p = policy();
        let dms = DmBindings {
            recipients: ["1", "2", "3"]
                .iter()
                .map(|id| DmBinding {
                    user_id: id.parse().unwrap(),
                    channel_id: "100".parse().unwrap(),
                })
                .collect(),
        };
        assert_eq!(
            p.authorize_with_dms(&actor("1"), Action::Improve, &dms),
            Ok(Workspace::ControlSource)
        );
        for id in ["2", "3"] {
            assert_eq!(
                p.authorize_with_dms(&actor(id), Action::Task, &dms),
                Ok(Workspace::User)
            );
            assert!(p
                .authorize_with_dms(&actor(id), Action::Improve, &dms)
                .is_err());
        }
        assert!(p.authorize(&actor("4"), Action::Task).is_err());
        assert!(p.authorize(&Actor::Unknown, Action::Improve).is_err());
    }
    #[test]
    fn channel_duplicates_and_revocation_fail_closed() {
        let mut p = policy();
        assert!(p.bind("vivkelly", "1".parse().unwrap()).is_err());
        assert!(p
            .authorize(
                &Actor::Discord {
                    user_id: "1".parse().unwrap(),
                    channel_id: "101".parse().unwrap()
                },
                Action::Improve
            )
            .is_err());
        p.principals[0].user_id = None;
        assert!(p.authorize(&actor("1"), Action::Improve).is_err());
    }
    #[test]
    fn server_access_cannot_be_enabled_in_policy() {
        let mut p = policy();
        p.channel_id = Some("100".parse().unwrap());
        assert!(p.validate().is_err());
        assert!(policy()
            .authorize(
                &Actor::Discord {
                    user_id: "1".parse().unwrap(),
                    channel_id: "100".parse().unwrap()
                },
                Action::Task
            )
            .is_err());
    }
    #[test]
    fn direct_messages_need_matching_recipient_bindings_and_keep_roles() {
        let mut p = policy();
        p.channel_id = None;
        let dms = DmBindings {
            recipients: vec![
                DmBinding {
                    user_id: "1".parse().unwrap(),
                    channel_id: "101".parse().unwrap(),
                },
                DmBinding {
                    user_id: "2".parse().unwrap(),
                    channel_id: "102".parse().unwrap(),
                },
            ],
        };
        let actor = |user: &str, channel: &str| Actor::DiscordDm {
            user_id: user.parse().unwrap(),
            channel_id: channel.parse().unwrap(),
        };
        assert_eq!(
            p.authorize_with_dms(&actor("1", "101"), Action::Improve, &dms),
            Ok(Workspace::ControlSource)
        );
        assert_eq!(
            p.authorize_with_dms(&actor("2", "102"), Action::Task, &dms),
            Ok(Workspace::User)
        );
        assert!(p
            .authorize_with_dms(&actor("2", "102"), Action::Improve, &dms)
            .is_err());
        assert!(p
            .authorize_with_dms(&actor("2", "101"), Action::Task, &dms)
            .is_err());
        assert!(p
            .authorize_with_dms(&actor("3", "103"), Action::Task, &dms)
            .is_err());
        p.principals[0].user_id = None;
        assert!(p
            .authorize_with_dms(&actor("1", "101"), Action::Improve, &dms)
            .is_err());
    }
}
