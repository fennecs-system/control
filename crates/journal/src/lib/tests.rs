
use super::*;
use control_comonad::{Store, cut};
use serde::Deserialize;

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("control-journal-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("history.jsonl")
}

#[test]
fn appends_are_durable_lines_that_reopen_at_the_same_positions() {
    let path = scratch("reopen");
    let mut journal = FileJournal::open(&path).unwrap();
    assert_eq!(journal.append(br#"{"turn":0}"#).unwrap(), 0);
    assert_eq!(journal.append(br#"{"turn":1,"text":"hi"}"#).unwrap(), 1);
    assert!(
        journal.append(b"{\"a\":\n1}").is_err()
            && journal.append(b"not json").is_err()
            && journal.append(b"").is_err()
    );
    drop(journal);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "{\"turn\":0}\n{\"turn\":1,\"text\":\"hi\"}\n",
        "plain JSON lines on disk"
    );
    let mut journal = FileJournal::open(&path).unwrap();
    assert_eq!(
        (journal.len(), journal.read(1).unwrap().as_deref()),
        (2, Some(&br#"{"turn":1,"text":"hi"}"#[..]))
    );
    assert_eq!(journal.append(br#"{"turn":2}"#).unwrap(), 2);
    assert_eq!(journal.read(9).unwrap(), None);
    assert_eq!(
        read_line(&path, 2).unwrap().as_deref(),
        Some(&br#"{"turn":2}"#[..]),
        "readers need no handle"
    );
    assert_eq!(read_line(&path, 3).unwrap(), None);
    journal.truncate(1).unwrap();
    assert_eq!(
        (
            journal.len(),
            journal.append(br#"{"turn":"again"}"#).unwrap()
        ),
        (1, 1),
        "truncation undoes uncommitted appends"
    );
    drop(journal);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "{\"turn\":0}\n{\"turn\":\"again\"}\n"
    );
}

#[test]
fn one_handle_owns_a_journal_at_a_time() {
    let path = scratch("lock");
    let journal = FileJournal::open(&path).unwrap();
    assert_eq!(
        FileJournal::open(&path).err().map(|e| e.kind()),
        Some(io::ErrorKind::WouldBlock)
    );
    drop(journal);
    assert!(
        FileJournal::open(&path).is_ok(),
        "the lock ends with the handle"
    );
}

#[test]
fn a_torn_final_line_is_dropped_on_open() {
    let path = scratch("torn");
    std::fs::write(&path, "{\"turn\":0}\n{\"turn\":1}\n{\"tur").unwrap();
    let mut journal = FileJournal::open(&path).unwrap();
    assert_eq!(journal.len(), 2);
    journal.append(br#"{"turn":2}"#).unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "{\"turn\":0}\n{\"turn\":1}\n{\"turn\":2}\n"
    );
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Turn {
    n: u64,
    text: String,
}

#[test]
fn typed_journals_are_comonad_spaces() {
    let mut typed = Typed::new(Memory::default());
    for n in 0..100 {
        typed
            .append(&Turn {
                n,
                text: format!("turn {n}"),
            })
            .unwrap();
    }
    let focus = Store::new(&typed, 99).unwrap();
    assert_eq!(
        focus.extract(),
        Turn {
            n: 99,
            text: "turn 99".into()
        }
    );
    assert_eq!(focus.peek(3).map(|t| t.n), Some(3));
    let view: Vec<u64> = cut(typed.len(), 4).map(|b| b.start).collect();
    assert_eq!(
        &view[..4],
        &[99, 98, 97, 96],
        "verbatim at the focus, coarser behind"
    );
    let file = FileJournal::open(&scratch("space")).unwrap();
    let mut typed = Typed::<_, Turn>::new(file);
    typed
        .append(&Turn {
            n: 7,
            text: "on disk".into(),
        })
        .unwrap();
    assert_eq!(Store::new(&typed, 0).unwrap().extract().text, "on disk");
}
