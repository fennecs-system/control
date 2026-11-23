use super::*;
use crate::services::{
    Phase, Storage,
    tests::{admit_fixture, fixture, inspection},
};
use serde_json::json;

/// A named mutation that must make a record unacceptable.
type Change<T> = (&'static str, fn(&mut T));

fn service() -> Service {
    let (mut state, run, _, time) = fixture();
    admit_fixture(&mut state, &run, time, |_| Ok(())).unwrap();
    state.services.remove(0)
}

#[test]
fn replacements_reopen_the_same_chat_workspace_but_other_owners_do_not() {
    let original = service();
    let name = Workspace::of(&original).unwrap().name;
    let mut replacement = original.binding_snapshot();
    replacement.resource.id = "replacement".to_owned().try_into().unwrap();
    assert_eq!(Workspace::of(&replacement).unwrap().name(), name);
    assert_eq!(original.summary()["workspace"]["volume"], name);
    replacement.resource.owner.chat = "other-chat".to_owned().try_into().unwrap();
    assert_ne!(Workspace::of(&replacement).unwrap().name(), name);
    replacement.resource.owner = original.resource.owner.clone();
    replacement.resource.owner.node = "other-node".to_owned().try_into().unwrap();
    assert_ne!(Workspace::of(&replacement).unwrap().name(), name);
}

#[test]
fn volume_identity_rejects_foreign_owners_and_host_backed_driver_options() {
    let service = service();
    let workspace = Workspace::of(&service).unwrap();
    let record = ObservedVolume {
        name: workspace.name().into(),
        driver: "local".into(),
        scope: "local".into(),
        labels: workspace.labels().into_iter().map(|(k, v)| (k.to_owned(), v.to_owned())).collect(),
        options: Default::default(),
    };
    assert!(workspace.matches(&record));
    let changes: [Change<ObservedVolume>; 5] = [
        ("Name", |v| v.name = "foreign".into()),
        ("Driver", |v| v.driver = "plugin".into()),
        ("Scope", |v| v.scope = "global".into()),
        ("Labels", |v| v.labels.clear()),
        ("Options", |v| v.options = [("type", "none"), ("o", "bind"), ("device", "/host")].map(|(k, v)| (k.to_owned(), v.to_owned())).into()),
    ];
    for (field, change) in changes {
        let mut bad = record.clone();
        change(&mut bad);
        assert!(!workspace.matches(&bad), "{field}");
    }
    for field in [
        "dev.control.node",
        "dev.control.chat",
        "dev.control.purpose",
    ] {
        let mut bad = record.clone();
        bad.labels.insert(field.into(), "other".into());
        assert!(!workspace.matches(&bad), "{field}");
    }
}

#[test]
fn container_mounts_require_exact_workspace_and_reject_extra_or_missing_mounts() {
    let mut service = service();
    service.resource.phase = Phase::Creating;
    let good = inspection(&service, &"b".repeat(64));
    assert!(crate::services::verify(&service, &good).is_ok());
    let tmpfs = crate::services::tmpfs(&service);
    assert!(!valid_mounts(&service, &[], &tmpfs));
    let mount = good.mounts[0].clone();
    assert!(!valid_mounts(
        &service,
        &[mount.clone(), mount.clone()],
        &tmpfs
    ));
    let changes: [Change<ObservedMount>; 5] = [
        ("Type", |m| m.kind = MountKind::Bind),
        ("Name", |m| m.name = Some("foreign".into())),
        ("Driver", |m| m.driver = Some("plugin".into())),
        ("Destination", |m| m.destination = "/home/agent".into()),
        ("RW", |m| m.writable = false),
    ];
    for (field, change) in changes {
        let mut bad = good.clone();
        change(&mut bad.mounts[0]);
        assert!(crate::services::verify(&service, &bad).is_err(), "{field}");
    }
    let mut bad = good.clone();
    bad.mounts.push(ObservedMount { kind: MountKind::Bind, name: None, driver: None, source: String::new(), destination: "/host".into(), writable: true });
    assert!(crate::services::verify(&service, &bad).is_err());
}

#[test]
fn restoring_legacy_disk_records_does_not_adopt_or_require_a_workspace_volume() {
    let service = service();
    let mut record = serde_json::to_value(&service).unwrap();
    record["storage"]["kind"] = json!("disk");
    let mut legacy: Service = serde_json::from_value(record).unwrap();
    assert_eq!(legacy.storage, Storage::Disk { limit_mb: None });
    assert!(Workspace::of(&legacy).is_none());
    assert!(legacy.summary()["workspace"].is_null());
    legacy.resource.phase = Phase::Creating;
    assert!(crate::services::verify(&legacy, &inspection(&legacy, &"b".repeat(64))).is_ok());
    assert!(!valid_mounts(
        &legacy,
        &inspection(&service, &"b".repeat(64)).mounts,
        &crate::services::tmpfs(&legacy)
    ));
}
