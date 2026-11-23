use super::*;
use control_isolation::RunSpec;

const OWNER: User = User { uid: 501, gid: 20 };
static FOLDER: Folder = Folder {
    target: "/home/agent/.cli-login",
    file: "auth.json",
    variable: "CLI_AUTH_PATH",
    names: Names::File,
    keep: &[
        Keep::Name("auth.json"),
        Keep::Name("auth.json.lock"),
        Keep::Between(".", ".tmp"),
    ],
};

#[test]
fn a_sweep_keeps_the_sign_in_its_lock_and_temporary_copies_only() {
    let entries = [
        "auth.json",
        "auth.json.lock",
        ".41.7.tmp",
        ".tmp",
        "sessions",
        "config.toml",
        "auth.json.bak",
    ];
    assert_eq!(
        sweep_plan(entries, FOLDER.keep),
        [".tmp", "sessions", "config.toml", "auth.json.bak"]
    );
}

#[test]
fn a_sweep_removes_files_folders_and_links_but_follows_no_link() {
    let root = std::env::temp_dir().join(format!("control-sweep-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let (folder, outside) = (root.join("login"), root.join("outside"));
    fs::create_dir_all(folder.join("sessions/today")).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("kept"), "x").unwrap();
    fs::write(folder.join("auth.json"), "refreshed").unwrap();
    fs::write(folder.join("sessions/today/s.jsonl"), "x").unwrap();
    fs::write(folder.join("history"), "x").unwrap();
    std::os::unix::fs::symlink(&outside, folder.join("link")).unwrap();
    sweep(&folder, FOLDER.keep).unwrap();
    let left: Vec<_> = fs::read_dir(&folder)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(left, ["auth.json"]);
    assert!(outside.join("kept").exists(), "the link's target is untouched");
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn only_a_refreshable_sign_in_is_mounted_writable_and_never_a_key() {
    let spec = || RunSpec::default();
    let path = PathBuf::from("/host/accounts/cli/second/auth.json");
    let file = "/home/agent/.cli/auth.json";
    // A key, a cloud credential and a read-only sign-in: the file, read-only.
    for granted in [
        Granted::located(Form::ApiKey, path.clone(), OWNER),
        Granted::located(Form::CloudDefault, path.clone(), OWNER),
        Granted::located(Form::Session, path.clone(), OWNER),
    ] {
        for keeps in [Keeps::InPlace, Keeps::Folder(&FOLDER)] {
            let mounted = granted.mount_session(spec(), "cli/login", file, keeps).unwrap();
            let [secret] = mounted.secrets.as_slice() else { panic!() };
            assert_eq!((secret.target.as_str(), secret.access), (file, Access::ReadOnly));
            assert_eq!(secret.source, path);
            assert!(mounted.env.is_empty());
            assert_eq!(granted.folder(keeps), None);
            assert_eq!(granted.session_target(file, keeps), file);
        }
    }
    let refreshable = Granted::refreshable(path.clone(), OWNER);
    assert_eq!(refreshable.form(), Form::Session);
    // Rewritten in place: the file itself, writable.
    let in_place = refreshable
        .mount_session(spec(), "cli/login", file, Keeps::InPlace)
        .unwrap();
    assert_eq!(
        (
            in_place.secrets[0].target.as_str(),
            in_place.secrets[0].access,
            &in_place.secrets[0].source
        ),
        (file, Access::ReadWrite, &path)
    );
    // Replaced by rename: its own folder, writable, and the CLI pointed there.
    let folder = refreshable
        .mount_session(spec(), "cli/login", file, Keeps::Folder(&FOLDER))
        .unwrap();
    let [secret] = folder.secrets.as_slice() else { panic!() };
    assert_eq!(
        (secret.target.as_str(), secret.access, secret.source.as_path()),
        (
            "/home/agent/.cli-login",
            Access::ReadWrite,
            Path::new("/host/accounts/cli/second")
        )
    );
    assert_eq!(
        folder.env,
        [(
            "CLI_AUTH_PATH".to_owned(),
            "/home/agent/.cli-login/auth.json".to_owned()
        )]
    );
    assert_eq!(
        refreshable.session_target(file, Keeps::Folder(&FOLDER)),
        "/home/agent/.cli-login/auth.json"
    );
    // A file the CLI would not find in its folder is refused, not mounted elsewhere.
    let misnamed = Granted::refreshable("/host/claude/other.json".into(), OWNER);
    assert!(
        misnamed
            .mount_session(spec(), "cli/login", file, Keeps::Folder(&FOLDER))
            .is_err()
    );
}
