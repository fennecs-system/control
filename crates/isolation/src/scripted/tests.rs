use super::*;
use crate::{Access, Limits, Stream, Tmpfs, User};
use std::io::Read;

fn service() -> RunSpec {
    RunSpec {
        name: "control-service-1".into(),
        image: "sha256:img".into(),
        entrypoint: Some("control-service-runner".into()),
        user: Some(User { uid: 501, gid: 20 }),
        network: Network::None,
        limits: Limits {
            pids: Some(64),
            memory: Some(Size::Mib(512)),
            cpus: Some(1),
            ..Limits::default()
        },
        disk: Some(Size::Gib(1)),
        ..RunSpec::default()
    }
    .label("dev.control.node", "n")
    .bind("/host/work", "/workspace", Access::ReadWrite)
    .secret("token", "/host/token", "/run/token", Access::ReadOnly)
    .tmpfs(Tmpfs::new("/tmp", Size::Mib(64)))
    .args(["sleep", "infinity"])
}

/// The engine's view follows the containers' lifecycle, and inspection reports
/// what the spec asked for in the engine's own terms.
#[test]
fn the_model_follows_create_start_kill_and_remove() {
    let scripted = Scripted::new(|_: &Call| Answer::Model);
    let id = scripted.create(&service()).unwrap();
    assert_eq!(id.as_str().len(), 64);
    let [created] = <[_; 1]>::try_from(scripted.inspect(&[Target::Name("control-service-1")]).unwrap()).unwrap();
    assert_eq!((created.status.as_str(), created.running), ("created", false));
    assert_eq!(created.name, "/control-service-1");
    assert_eq!(created.user, "501:20");
    assert_eq!(created.network, "none");
    assert_eq!(created.memory, 512 << 20);
    assert_eq!(created.nano_cpus, 1_000_000_000);
    assert_eq!(created.tmpfs["/tmp"], "rw,nosuid,size=64m");
    assert_eq!(created.storage_opt.as_ref().unwrap()["size"], "1G");
    assert_eq!(created.label("dev.control.node"), Some("n"));
    let token = created.mount("/run/token").unwrap();
    assert_eq!((token.source.as_str(), token.writable), ("/host/token", false));
    assert!(created.mount("/workspace").unwrap().writable);
    assert!(scripted.create(&service()).is_err(), "a taken name conflicts");
    assert!(scripted.list(States::Running, &Select::Any).unwrap().is_empty());
    scripted.start(&id).unwrap();
    assert_eq!(scripted.list(States::Running, &Select::Any).unwrap().len(), 1);
    let mut waiting = scripted.wait(&id).unwrap();
    assert!(
        waiting.try_wait().unwrap().is_none(),
        "wait runs while the container does"
    );
    scripted.kill(&id).unwrap();
    assert!(waiting.wait().unwrap().success(), "wait ends when the container stops");
    assert_eq!(scripted.inspect(&[Target::Id(&id)]).unwrap()[0].exit_code, 137);
    scripted.remove(Target::Id(&id)).unwrap();
    assert!(scripted.inspect(&[Target::Id(&id)]).is_err());
    assert_eq!(
        scripted.calls().first(),
        Some(&Call::Create(service())),
        "calls are recorded as typed data"
    );
}

/// The script serves program calls with local processes and can refuse any
/// call; specs the engine would reject are refused before the script sees them.
#[test]
fn programs_are_local_processes_and_refusals_keep_their_reason() {
    let scripted = Scripted::new(|call: &Call| match call {
        Call::Spawn(spec) => {
            let mut command = Command::new("sh");
            command.args(["-c", "printf '%s' \"$1\"", "sh", &spec.name]);
            Answer::Run(command)
        }
        Call::Create(_) => Answer::Refuse("storage driver has no quota".into()),
        _ => Answer::Model,
    });
    let streams = Streams::new(Stream::Null, Stream::Piped, Stream::Null);
    let mut child = scripted.spawn(&service(), streams).unwrap();
    let mut output = String::new();
    child.stdout.take().unwrap().read_to_string(&mut output).unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(output, "control-service-1");
    let refused = scripted.create(&service()).unwrap_err();
    assert_eq!(refused.to_string(), "storage driver has no quota");
    assert!(scripted.model().containers.is_empty());
    let unsafe_path = service().bind("/host/a,b", "/b", Access::ReadOnly);
    assert!(scripted.spawn(&unsafe_path, streams).is_err());
    assert_eq!(scripted.calls().len(), 2, "a rejected spec never reaches the engine");
}

#[test]
fn volumes_keep_labels_and_refuse_removal_while_mounted() {
    let scripted = Scripted::new(|_: &Call| Answer::Model);
    scripted.volume_create("chat-1", &[("dev.control.chat", "1")]).unwrap();
    scripted.volume_create("chat-1", &[("dev.control.chat", "2")]).unwrap();
    assert_eq!(
        scripted.volume_inspect("chat-1").unwrap()[0].labels["dev.control.chat"],
        "1"
    );
    assert_eq!(scripted.volumes(&[("dev.control.chat", "1")]).unwrap().len(), 1);
    let mut spec = service();
    spec.mounts.push(crate::Mount {
        source: Source::Volume("chat-1".into()),
        target: "/data".into(),
        access: Access::ReadWrite,
    });
    let id = scripted.create(&spec).unwrap();
    assert!(scripted.volume_remove("chat-1").is_err());
    scripted.remove(Target::Id(&id)).unwrap();
    scripted.volume_remove("chat-1").unwrap();
    assert!(scripted.volumes(&[]).unwrap().is_empty());
}
