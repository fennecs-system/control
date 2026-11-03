//! Recording a turn as history grows: a durable journal append against a
//! whole-state rewrite, plus reads and the one-time index scan on open.
//! `cargo run --release --example bench [dir]`.
use control_journal::{FileJournal, Journal};
use std::{fs, hint::black_box, io::Write, path::PathBuf, time::Instant};

fn entry(n: u64) -> Vec<u8> {
    format!(r#"{{"turn":{n},"role":"assistant","text":"{}"}}"#, "The server is ready and both bots joined. ".repeat(40)).into_bytes()
}

fn main() {
    let dir = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| std::env::temp_dir().to_string_lossy().into_owned())).join("control-journal-bench");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    println!("{:>8}  {:>14}  {:>16}  {:>10}  {:>10}", "history", "journal append", "whole rewrite", "read", "open scan");
    let mut history = Vec::new();
    let path = dir.join("chat.jsonl");
    let mut journal = FileJournal::open(&path).unwrap();
    for target in [1_000u64, 10_000, 100_000] {
        // Bulk fill without per-line sync; the measured appends below sync.
        while (history.len() as u64) < target {
            history.push(entry(history.len() as u64));
        }
        if journal.len() < target {
            let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
            let start = journal.len() as usize;
            for e in &history[start..] {
                file.write_all(e).unwrap();
                file.write_all(b"\n").unwrap();
            }
            file.sync_all().unwrap();
            journal = FileJournal::open(&path).unwrap();
        }
        let rounds = 50;
        let start = Instant::now();
        for _ in 0..rounds {
            let e = entry(journal.len());
            journal.append(&e).unwrap();
            history.push(e);
        }
        let append = start.elapsed() / rounds;
        // Today's model: serialise the whole history and rewrite it durably.
        let state = dir.join("state.json");
        let start = Instant::now();
        for _ in 0..5 {
            let data = serde_json::to_vec(&history.iter().map(|e| serde_json::from_slice::<serde_json::Value>(e).unwrap()).collect::<Vec<_>>()).unwrap();
            let tmp = dir.join("state.json.tmp");
            let mut file = fs::File::create(&tmp).unwrap();
            file.write_all(&data).unwrap();
            file.sync_all().unwrap();
            fs::rename(&tmp, &state).unwrap();
        }
        let rewrite = start.elapsed() / 5;
        let start = Instant::now();
        let mut bytes = 0;
        for i in 0..1000u64 {
            bytes += journal.read((i * 7919) % journal.len()).unwrap().unwrap().len();
        }
        let read = start.elapsed() / 1000;
        black_box(bytes);
        let start = Instant::now();
        let reopened = FileJournal::open(&path).unwrap();
        let open = start.elapsed();
        assert_eq!(reopened.len(), journal.len());
        println!("{:>8}  {:>14?}  {:>16?}  {:>10?}  {:>10?}", journal.len(), append, rewrite, read, open);
    }
    let _ = fs::remove_dir_all(&dir);
}
