//! What each kind of write re-encodes on its save: the values the save's
//! split encoded afresh (`snapshot::fresh`), by path. A write that borrows a
//! part mutably without changing it throws its saved encoding away, and it
//! shows up here.
use super::*;

const SESSION: &str = "fresh-encoding-operator-session";

/// A form post through the whole request handler, on this thread so the
/// save's split records here. Answers the status line.
fn post(store: &crate::store::Store, target: &str, body: &str) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let mut client = std::net::TcpStream::connect(address).unwrap();
    let (server, _) = listener.accept().unwrap();
    write!(
        client,
        "POST {target} HTTP/1.1\r\nHost: {address}\r\nCookie: control_operator={SESSION}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    crate::handle(
        server,
        store.clone(),
        std::sync::Arc::new(crate::access::Access::for_tests(SESSION)),
    );
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    response.lines().next().unwrap_or_default().to_owned()
}

/// The kinds of write in the end-to-end write mix, plus a run's status
/// changes, each applied to `store` and saved. `round` makes each distinct.
/// A write applied to the store: in the chat, distinct per round.
type Apply = Box<dyn Fn(&crate::store::Store, &str, usize)>;

fn writes() -> Vec<(&'static str, Apply)> {
    vec![
        (
            "room post",
            Box::new(|store, _, round| {
                let status = post(store, "/room", &format!("channel=research&message=write+{round}"));
                assert!(status.contains("303"), "{status}");
            }),
        ),
        (
            "spawn",
            Box::new(|store, _, round| {
                let status = post(
                    store,
                    "/spawn",
                    &format!("objective=write+{round}&provider=Self-hosted"),
                );
                assert!(status.contains("303"), "{status}");
            }),
        ),
        (
            "gene edit",
            Box::new(|store, _, round| {
                let status = post(
                    store,
                    "/dna/save",
                    &format!("id=fresh-{round}&name=Fresh+{round}&revision=0&instructions=Written+by+the+write+mix"),
                );
                assert!(status.contains("303"), "{status}");
            }),
        ),
        (
            "chat message",
            Box::new(|store, chat, _| {
                let status = post(store, &format!("/dm/{chat}/messages"), "message=%2Fimprove+off");
                assert!(status.contains("303"), "{status}");
            }),
        ),
        (
            "task post",
            Box::new(|store, _, round| {
                let status = post(store, "/task", &format!("objective=write+{round}"));
                assert!(status.contains("303"), "{status}");
            }),
        ),
        (
            "run starts",
            Box::new(|store, chat, round| {
                // Queued and saved first, so only the dispatch's save is counted.
                store
                    .write(|state| {
                        crate::chat::send(state, chat, &format!("turn {round}"), "test").unwrap();
                        save(state).unwrap();
                        drop(snapshot::fresh::take());
                        crate::worker::checkpoint_next(state, save).expect("a queued turn")
                    })
                    .unwrap();
            }),
        ),
    ]
}

/// A value a save encoded afresh: where, its length, and whether it differs
/// from the value there before the write (if not, the write borrowed it
/// mutably without changing it, or it sits beside a change in a part).
struct Fresh {
    path: String,
    bytes: usize,
    changed: bool,
}

/// Each write kind's fresh encodings on its second round (the first warms
/// whatever it creates once).
fn fresh_by_write(store: &crate::store::Store, chat: &str) -> Vec<(&'static str, Vec<Fresh>)> {
    store.write(|state| save(state).unwrap()).unwrap();
    store.write(|state| save(state).unwrap()).unwrap();
    let directory = crate::config::state_directory().unwrap();
    let document = || {
        let tree = store
            .read(|state| encode(state, &archived(&directory)).unwrap())
            .unwrap();
        serde_json::from_slice::<serde_json::Value>(&tree.bytes()).unwrap()
    };
    writes()
        .into_iter()
        .map(|(kind, write)| {
            write(store, chat, 0);
            let before = document();
            drop(snapshot::fresh::take());
            write(store, chat, 1);
            let fresh = snapshot::fresh::take();
            let after = document();
            let fresh = fresh
                .into_iter()
                .map(|(path, bytes)| {
                    let at = format!("/{path}");
                    let changed = before.pointer(&at) != after.pointer(&at);
                    Fresh { path, bytes, changed }
                })
                .collect();
            (kind, fresh)
        })
        .collect()
}

fn fixture(dms: usize, turns: usize) -> (crate::store::Store, String) {
    let mut state = crate::performance::history_fixture(dms, turns);
    for run in state.runtime.runs.iter_mut() {
        run.emitted = Some(run.status.clone());
    }
    let chat = crate::chat::create(&mut state);
    (crate::store::Store::new(state), chat)
}

fn table(fresh: &[(&str, Vec<Fresh>)]) -> String {
    fresh
        .iter()
        .map(|(kind, values)| {
            let paths = |changed: bool| {
                let mut paths: Vec<&str> = values
                    .iter()
                    .filter(|value| value.changed == changed)
                    .map(|value| value.path.as_str())
                    .collect();
                paths.sort_unstable();
                paths.join(" ")
            };
            format!(
                "{kind}\t{}\t{}\t{}\t{}\n",
                values.len(),
                values.iter().map(|value| value.bytes).sum::<usize>(),
                paths(true),
                paths(false)
            )
        })
        .collect()
}

/// Per write kind on the seeded history fixture (1,000 DMs, 20,000 turns):
/// values re-encoded, their bytes, and their paths.
#[test]
#[ignore = "debug-build fixture: run alone with scratch CONTROL_STATE_DIR/CONTROL_CONFIG_DIR; prints a table"]
fn profile_fresh_encodings_by_write() {
    let (store, chat) = fixture(1000, 20_000);
    print!(
        "write\tvalues\tbytes\tchanged\tunchanged\n{}",
        table(&fresh_by_write(&store, &chat))
    );
}

/// Each write in the write mix re-encodes one value, the one it changed;
/// the runtime's notice comes along when the runtime itself is re-split.
#[test]
fn each_write_re_encodes_only_what_it_changed() {
    let (store, chat) = fixture(20, 60);
    for (kind, values) in fresh_by_write(&store, &chat) {
        let changed: Vec<&str> = values
            .iter()
            .filter(|value| value.changed)
            .map(|value| value.path.as_str())
            .collect();
        let expected = match kind {
            "room post" => "collective/posts/",
            "spawn" => "agents/",
            "gene edit" => "lens_library",
            "chat message" => "chats/",
            _ => "runtime/runs/",
        };
        assert!(
            changed.len() == 1 && changed[0].starts_with(expected),
            "{kind} re-encoded {changed:?}"
        );
        let unchanged: Vec<&str> = values
            .iter()
            .filter(|value| !value.changed && value.path != "runtime/notice")
            .map(|value| value.path.as_str())
            .collect();
        assert!(unchanged.is_empty(), "{kind} re-encoded unchanged {unchanged:?}");
    }
}
