use super::*;

const ID: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn container(extra_host: &str, mounts: &str, ports: &str) -> String {
    format!(
        r#"[{{"Id":"{ID}","Name":"/control-run-1","Image":"sha256:img",
        "Config":{{"Image":"sha256:ref","Labels":{{"dev.control.node":"n","dev.control.attempt":"control-run-1"}},"User":"501:20","Entrypoint":["control-service-runner"],"Cmd":["1","2","sleep 1"]}},
        "State":{{"Status":"running","Running":true,"ExitCode":0}},
        "HostConfig":{{"NetworkMode":"none","ReadonlyRootfs":true,"Privileged":false,"PidMode":"","RestartPolicy":{{"Name":"no"}},"CapDrop":["ALL"],"SecurityOpt":["no-new-privileges"],"Memory":536870912,"NanoCpus":1000000000,"PidsLimit":64{extra_host}}},
        "Mounts":{mounts},"NetworkSettings":{{"Ports":{ports}}}}}]"#
    )
}

#[test]
fn an_inspection_becomes_typed_facts() {
    let text = container(
        r#","Tmpfs":{"/tmp":"rw,size=1m"},"StorageOpt":{"size":"512M"}"#,
        r#"[{"Type":"bind","Source":"/w","Destination":"/workspace","RW":false},{"Type":"volume","Name":"v","Driver":"local","Source":"/var/lib/v","Destination":"/data","RW":true}]"#,
        r#"{"8080/tcp":null}"#,
    );
    let [observed] = <[Observed; 1]>::try_from(inspection(&text).unwrap()).unwrap();
    assert_eq!(observed.id.as_str(), ID);
    assert_eq!(
        (
            observed.name.as_str(),
            observed.image.as_str(),
            observed.image_ref.as_str()
        ),
        ("/control-run-1", "sha256:img", "sha256:ref")
    );
    assert_eq!(observed.label("dev.control.node"), Some("n"));
    assert_eq!(
        (observed.status.as_str(), observed.running, observed.exit_code),
        ("running", true, 0)
    );
    assert_eq!(
        (
            observed.network.as_str(),
            observed.read_only_root,
            observed.privileged,
            observed.restart.as_str()
        ),
        ("none", true, false, "no")
    );
    assert_eq!(observed.cap_drop.as_deref(), Some(&["ALL".to_owned()][..]));
    assert_eq!(
        (observed.memory, observed.nano_cpus, observed.pids_limit),
        (536870912, 1000000000, Some(64))
    );
    assert_eq!(observed.tmpfs["/tmp"], "rw,size=1m");
    assert_eq!(observed.storage_opt.as_ref().unwrap()["size"], "512M");
    assert_eq!(
        observed.entrypoint.as_deref(),
        Some(&["control-service-runner".to_owned()][..])
    );
    let workspace = observed.mount("/workspace").unwrap();
    assert_eq!(
        (&workspace.kind, workspace.source.as_str(), workspace.writable),
        (&MountKind::Bind, "/w", false)
    );
    let data = observed.mount("/data").unwrap();
    assert_eq!(
        (&data.kind, data.name.as_deref(), data.driver.as_deref(), data.writable),
        (&MountKind::Volume, Some("v"), Some("local"), true)
    );
    assert_eq!(observed.ports, Some(BTreeMap::from([("8080/tcp".to_owned(), false)])));
}

#[test]
fn absent_engine_values_stay_absent() {
    let text = container(r#","Tmpfs":null"#, "[]", "null").replace(r#""CapDrop":["ALL"],"#, "");
    let text = text
        .replace(r#""Entrypoint":["control-service-runner"],"#, r#""Entrypoint":null,"#)
        .replace(r#""PidsLimit":64"#, r#""PidsLimit":null"#);
    let observed = inspection(&text).unwrap().remove(0);
    assert!(observed.tmpfs.is_empty() && observed.storage_opt.is_none() && observed.mount("/workspace").is_none());
    assert_eq!(
        (
            observed.cap_drop,
            observed.entrypoint,
            observed.pids_limit,
            observed.ports
        ),
        (None, None, None, None)
    );
    let published = container("", "[]", r#"{"8080/tcp":[{"HostIp":"0.0.0.0","HostPort":"80"}]}"#);
    assert_eq!(
        inspection(&published).unwrap()[0].ports,
        Some(BTreeMap::from([("8080/tcp".to_owned(), true)]))
    );
}

#[test]
fn malformed_engine_output_is_refused() {
    for text in ["", "{}", "not json", &container("", "[]", "{}").replace(ID, "short")] {
        assert!(inspection(text).is_err(), "{text}");
    }
    for id in ["", "abc", &"g".repeat(64), &format!("{ID}0")] {
        assert!(container_id(id).is_err(), "{id}");
    }
    assert_eq!(container_id(&format!("{ID}\n")).unwrap().as_str(), ID);
}

#[test]
fn listings_parse_ids_names_and_labels() {
    let text = format!("{ID}\tcontrol-run-1\tdev.control.node=n,dev.control.purpose=run\n{ID}\tother\t\n\n");
    let listed = listing(&text).unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(
        (
            listed[0].name.as_str(),
            listed[0].labels.get("dev.control.purpose").map(String::as_str)
        ),
        ("control-run-1", Some("run"))
    );
    assert!(listed[1].labels.is_empty());
    assert!(listing("short\tname\t").is_err());
    assert!(listing("").unwrap().is_empty());
    let volumes = volume_listing("control-service-workspace-a\tdev.control.chat=c,dev.control.node=n\nbare\n").unwrap();
    assert_eq!(
        (volumes[0].labels["dev.control.chat"].as_str(), volumes[1].name.as_str()),
        ("c", "bare")
    );
}

#[test]
fn volume_and_image_inspections_parse() {
    let observed =
        volumes(r#"[{"Name":"v","Driver":"local","Scope":"local","Labels":{"a":"b"},"Options":null}]"#).unwrap();
    assert_eq!(
        observed,
        [ObservedVolume {
            name: "v".into(),
            driver: "local".into(),
            scope: "local".into(),
            labels: BTreeMap::from([("a".into(), "b".into())]),
            options: BTreeMap::new()
        }]
    );
    assert!(volumes("[]").unwrap().is_empty() && volumes("x").is_err());
    assert_eq!(
        image_labels(r#"[{"Config":{"Labels":{"dev.control.tcp-relay":"5"}}}]"#).unwrap()["dev.control.tcp-relay"],
        "5"
    );
    assert!(image_labels(r#"[{"Config":{"Labels":null}}]"#).unwrap().is_empty());
    assert!(image_labels("[]").is_err() && image_labels("[{},{}]").is_err());
}

#[test]
fn storage_quota_follows_the_driver_and_backing_filesystem() {
    assert!(quota_capable(
        "overlay2 [[\"Backing Filesystem\",\"xfs\"],[\"Supports d_type\",\"true\"]]"
    ));
    assert!(!quota_capable("overlay2 [[\"Backing Filesystem\",\"extfs\"]]"));
    assert!(!quota_capable(
        "overlayfs [[\"driver-type\",\"io.containerd.snapshotter.v1\"]]"
    ));
    assert!(quota_capable("btrfs null\n"));
    assert!(!quota_capable(""));
}

#[test]
fn stats_and_processes_parse_engine_output() {
    let stats = stats("abc\t1.25%\t200MiB / 2GiB\t12\nbad line\n");
    assert_eq!(stats.len(), 1);
    let sample = &stats["abc"];
    assert_eq!(
        (sample.cpu.as_str(), sample.memory.as_str(), sample.pids.as_str()),
        ("1.25%", "200MiB / 2GiB", "12")
    );
    let processes = top(
        "PID    USER   COMMAND\n1      502    control-service-runner 0 0 bash -lc 'sleep infinity'\n77     502    java -Xmx1G -jar server.jar nogui\n90 502 sleep\n",
    );
    assert_eq!(processes.len(), 3);
    assert_eq!(
        (
            processes[1].pid.as_str(),
            processes[1].user.as_str(),
            processes[1].command.as_str()
        ),
        ("77", "502", "java -Xmx1G -jar server.jar nogui")
    );
    assert_eq!(top("PID USER COMMAND\n5 502\n")[0].command, "");
}
