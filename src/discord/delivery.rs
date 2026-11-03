//! Read projection of the persisted ledger. No application Runtime dependency.
//! Unknown fields are skipped by serde; history is folded while decoding rather
//! than retained as a JSON tree. This is a compatibility reader, not authority.
use crate::identity::{Action, Actor};
use serde::{
    Deserialize, Deserializer,
    de::{SeqAccess, Visitor},
};
use serde_json::Value;
use std::{collections::HashSet, fmt, marker::PhantomData};

#[derive(Default, Deserialize)]
#[serde(default)]
pub(super) struct Snapshot {
    pub runtime: Runtime,
    pub discord_seen: HashSet<String>,
}
#[derive(Default, Deserialize)]
#[serde(default)]
pub(super) struct Runtime {
    pub runs: Vec<Run>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub(super) struct Run {
    pub id: String,
    pub source: String,
    pub chat_id: Option<String>,
    pub actor: Actor,
    pub action: Action,
    pub status: String,
    pub result: Option<String>,
    pub question: Value,
    #[serde(rename = "attempts", deserialize_with = "last_attempt")]
    pub latest: Option<Attempt>,
}
impl Run {
    /// The latest attempt's last edit, from state or, once the attempt has
    /// finished, from its chat's history journal (read without a lock).
    pub fn change(&self) -> Option<Change> {
        let attempt = self.latest.as_ref()?;
        if attempt.change.is_some() {
            return attempt.change.clone();
        }
        let name = self.chat_id.as_deref().filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')).unwrap_or("_runs");
        let path = crate::config::state(&format!("history/{name}.jsonl")).ok()?;
        #[derive(Deserialize)]
        struct Entry {
            activity: Vec<Change>,
        }
        let line = control_journal::read_line(&path, attempt.journal?).ok()??;
        serde_json::from_slice::<Entry>(&line).ok()?.activity.into_iter().rev().find(|c| c.kind == "file_change" && !c.output.is_empty())
    }
}
#[derive(Default, Deserialize)]
#[serde(default)]
pub(super) struct Attempt {
    #[serde(rename = "activity", deserialize_with = "last_change")]
    pub change: Option<Change>,
    /// History-journal line holding a finished attempt's activity.
    pub journal: Option<u64>,
}
#[derive(Clone, Default, Deserialize)]
#[serde(default)]
pub(super) struct Change {
    pub kind: String,
    pub text: String,
    pub output: String,
}

fn last_attempt<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Attempt>, D::Error> {
    last(d, |_| true)
}
fn last_change<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Change>, D::Error> {
    last(d, |change: &Change| {
        change.kind == "file_change" && !change.output.is_empty()
    })
}
fn last<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
    keep: fn(&T) -> bool,
) -> Result<Option<T>, D::Error> {
    struct Fold<T> {
        keep: fn(&T) -> bool,
        marker: PhantomData<T>,
    }
    impl<'de, T: Deserialize<'de>> Visitor<'de> for Fold<T> {
        type Value = Option<T>;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("an event history")
        }
        fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<Self::Value, S::Error> {
            let mut selected = None;
            while let Some(value) = seq.next_element::<T>()? {
                if (self.keep)(&value) {
                    selected = Some(value);
                }
            }
            Ok(selected)
        }
    }
    d.deserialize_seq(Fold {
        keep,
        marker: PhantomData,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    #[ignore = "synthetic ledger CPU/peak-RSS comparison; no credentials or model calls"]
    fn profile_delivery_projection() {
        use std::{fs, hint::black_box, io::BufReader, process::Command, time::Instant};
        if let Ok(path) = std::env::var("CONTROL_PROFILE_LEDGER") {
            let mode = std::env::var("CONTROL_PROFILE_READER").unwrap();
            let start = Instant::now();
            for _ in 0..10 {
                if mode == "legacy" {
                    let bytes = fs::read(&path).unwrap();
                    black_box(serde_json::from_slice::<Value>(&bytes).unwrap());
                } else if mode == "stream" {
                    black_box(
                        serde_json::from_reader::<_, Snapshot>(BufReader::new(
                            fs::File::open(&path).unwrap(),
                        ))
                        .unwrap(),
                    );
                } else {
                    let bytes = crate::config::read_regular(
                        std::path::Path::new(&path),
                        isize::MAX as usize,
                        false,
                    )
                    .unwrap();
                    black_box(serde_json::from_slice::<Snapshot>(&bytes).unwrap());
                }
            }
            let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
            // SAFETY: getrusage initializes the valid output pointer on success.
            assert_eq!(
                unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
                0
            );
            let rss = unsafe { usage.assume_init() }.ru_maxrss as u64;
            let bytes = if cfg!(target_os = "macos") {
                rss
            } else {
                rss * 1024
            };
            eprintln!(
                "{mode}: 10 decodes in {:?}, peak RSS {} KiB",
                start.elapsed(),
                bytes / 1024
            );
            return;
        }
        let path = std::env::temp_dir().join(format!(
            "control-synthetic-ledger-{}.json",
            std::process::id()
        ));
        let activity: Vec<_> = (0..10).map(|index| json!({"kind":if index%2 == 0 {"file_change"} else {"command_execution"},"text":"cargo test","output":"x".repeat(1024)})).collect();
        let fixture = json!({"chats":(0..500).map(|_| json!({"messages":[{"content":"c".repeat(2048)}]})).collect::<Vec<_>>(),
            "runtime":{"runs":(0..250).map(|i| json!({"id":format!("r-{i}"),"source":"discord:2","status":"completed","result":"done","context":"context".repeat(600),
                "attempts":(0..3).map(|_| json!({"activity":activity,"model":"fixture"})).collect::<Vec<_>>() })).collect::<Vec<_>>()}});
        serde_json::to_writer(fs::File::create(&path).unwrap(), &fixture).unwrap();
        eprintln!(
            "Synthetic ledger: {} bytes; 250 runs, 750 attempts, 7500 activities",
            fs::metadata(&path).unwrap().len()
        );
        for _ in 0..3 {
            for mode in ["legacy", "stream", "typed"] {
                assert!(
                    Command::new(std::env::current_exe().unwrap())
                        .args([
                            "discord::delivery::tests::profile_delivery_projection",
                            "--ignored",
                            "--exact",
                            "--nocapture"
                        ])
                        .env("CONTROL_PROFILE_LEDGER", &path)
                        .env("CONTROL_PROFILE_READER", mode)
                        .status()
                        .unwrap()
                        .success()
                );
            }
        }
        fs::remove_file(path).unwrap();
    }
    #[test]
    fn projection_ignores_application_state_and_retains_only_latest_attempt_change() {
        let snapshot: Snapshot = serde_json::from_value(json!({
            "chats":[{"messages":[{"content":"private context not used by delivery"}]}],
            "discord_seen":["1","2","1"],
            "runtime":{"nodes":[{"unused":true}],"runs":[{"id":"r1","status":"completed",
                "attempts":[{"activity":[{"kind":"file_change","output":"old attempt"}]},
                    {"activity":[{"kind":"file_change","output":"old edit"},
                        {"kind":"file_change","text":"new edit","output":"+new"},
                        {"kind":"command_execution","output":"command output"},
                        {"kind":"file_change","output":""}]}]}]}
        }))
        .unwrap();
        assert_eq!(snapshot.discord_seen.len(), 2);
        let change = snapshot.runtime.runs[0]
            .latest
            .as_ref()
            .unwrap()
            .change
            .as_ref()
            .unwrap();
        assert_eq!(change.text, "new edit");
        assert_eq!(change.output, "+new");
    }
    #[test]
    fn empty_new_attempt_does_not_republish_old_changes() {
        let run: Run = serde_json::from_value(json!({"attempts":[
            {"activity":[{"kind":"file_change","output":"old"}]}, {"activity":[]}
        ]}))
        .unwrap();
        assert!(run.latest.unwrap().change.is_none());
    }
}
