use super::*;
use control_isolation::Access;

const OWNER: User = User { uid: 501, gid: 20 };

#[test]
fn the_shared_base_closes_the_home_to_other_uids() {
    let spec = base("control-run-1", "node", OWNER);
    assert_eq!(spec.user, Some(OWNER));
    let home = spec.tmpfs.iter().find(|fs| fs.target == "/home/agent").unwrap();
    assert_eq!((home.owner, home.mode), (Some(OWNER), Some(0o700)));
    assert!(spec.secrets.is_empty());
    let spec = spec.secret("k", "/k", "/home/agent/.k/key", Access::ReadOnly);
    assert_eq!(secrets_private(&spec), Ok(()));
}

#[test]
fn a_secret_outside_a_private_home_of_the_program_uid_is_named() {
    let secret = |spec: RunSpec, target: &str| spec.secret("k", "/k", target, Access::ReadOnly);
    // Outside any tmpfs, or under the open /tmp.
    let spec = secret(base("control-run-1", "node", OWNER), "/run/key");
    assert_eq!(secrets_private(&spec), Err("k".into()));
    let spec = secret(base("control-run-1", "node", OWNER), "/tmp/key");
    assert_eq!(secrets_private(&spec), Err("k".into()));
    // The home itself is not a file beneath it.
    let spec = secret(base("control-run-1", "node", OWNER), "/home/agent");
    assert_eq!(secrets_private(&spec), Err("k".into()));
    // A home owned by someone other than the program's uid.
    let other = RunSpec {
        user: Some(User { uid: 1000, gid: 1000 }),
        ..base("control-run-1", "node", OWNER)
    };
    assert_eq!(secrets_private(&secret(other, "/home/agent/key")), Err("k".into()));
    // A prefix that is not a parent directory.
    let spec = secret(base("control-run-1", "node", OWNER), "/home/agentx/key");
    assert_eq!(secrets_private(&spec), Err("k".into()));
}
