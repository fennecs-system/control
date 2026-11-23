
use super::*;
use std::fs;
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
fn alternate_identities_use_node_tasks_without_architect_authority() {
    // The old provider-owner setting is accepted but grants no role or access.
    let p: Policy = serde_json::from_str(r#"{"channel_id":null,"codex_owner_id":"2","principals":[{"label":"operator","user_id":"1","role":"architect"},{"label":"daily account","user_id":"2","role":"user"},{"label":"mobile account","user_id":"3","role":"user"}]}"#).unwrap();
    p.validate().unwrap();
    let dms = DmBindings {
        recipients: ["1", "2", "3"]
            .iter()
            .map(|id| DmBinding {
                user_id: id.parse().unwrap(),
                channel_id: "100".parse().unwrap(),
            })
            .collect(),
    };
    for id in ["2", "3"] {
        assert_eq!(
            p.authorize_with_dms(&actor(id), Action::Task, &dms),
            Ok(Workspace::User)
        );
        assert!(
            p.authorize_with_dms(&actor(id), Action::Improve, &dms)
                .is_err()
        );
        assert!(
            p.authorize_with_dms(&actor(id), Action::Task, &DmBindings::default())
                .is_err()
        );
    }
    assert!(
        p.authorize_with_dms(&actor("4"), Action::Task, &dms)
            .is_err()
    );
    assert!(
        !serde_json::to_string(&p)
            .unwrap()
            .contains("codex_owner_id")
    );
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
    use std::os::unix::fs::{PermissionsExt, symlink};
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
    p.save_in(&directory).unwrap();
    let path = directory.join("discord-access.json");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        Policy::read(&path).unwrap().principals[0].role,
        Role::Architect
    );
    let temporary = directory.join(format!("discord-access.json.{}.tmp", std::process::id()));
    fs::write(&temporary, b"preserve evidence").unwrap();
    p.principals[0].role = Role::User;
    assert!(p.save_in(&directory).is_err());
    assert_eq!(fs::read(&temporary).unwrap(), b"preserve evidence");
    assert_eq!(
        Policy::read(&path).unwrap().principals[0].role,
        Role::Architect
    );
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
        assert!(
            p.authorize_with_dms(&actor(id), Action::Improve, &dms)
                .is_err()
        );
    }
    assert!(p.authorize(&actor("4"), Action::Task).is_err());
    assert!(p.authorize(&Actor::Unknown, Action::Improve).is_err());
}
#[test]
fn channel_duplicates_and_revocation_fail_closed() {
    let mut p = policy();
    assert!(p.bind("vivkelly", "1".parse().unwrap()).is_err());
    assert!(
        p.authorize(
            &Actor::Discord {
                user_id: "1".parse().unwrap(),
                channel_id: "101".parse().unwrap()
            },
            Action::Improve
        )
        .is_err()
    );
    p.principals[0].user_id = None;
    assert!(p.authorize(&actor("1"), Action::Improve).is_err());
}
#[test]
fn server_access_cannot_be_enabled_in_policy() {
    let mut p = policy();
    p.channel_id = Some("100".parse().unwrap());
    assert!(p.validate().is_err());
    assert!(
        policy()
            .authorize(
                &Actor::Discord {
                    user_id: "1".parse().unwrap(),
                    channel_id: "100".parse().unwrap()
                },
                Action::Task
            )
            .is_err()
    );
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
    assert!(
        p.authorize_with_dms(&actor("2", "102"), Action::Improve, &dms)
            .is_err()
    );
    assert!(
        p.authorize_with_dms(&actor("2", "101"), Action::Task, &dms)
            .is_err()
    );
    assert!(
        p.authorize_with_dms(&actor("3", "103"), Action::Task, &dms)
            .is_err()
    );
    p.principals[0].user_id = None;
    assert!(
        p.authorize_with_dms(&actor("1", "101"), Action::Improve, &dms)
            .is_err()
    );
}
