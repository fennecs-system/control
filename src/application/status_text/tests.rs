//! Saved strings, record round trips and predicate sets for the statuses
//! declared with `status_enum!`. Record shapes are the fixtures' (command
//! jobs from main/tests/performance.rs, previews from previews/tests.rs,
//! publications from github/tests.rs, advisor facts from bots/tests/advisor.rs),
//! written in field order.
use crate::command_jobs::{Job, JobStatus};
use crate::github::{Publication, PublicationStatus};
use crate::previews::{Preview, PreviewStatus};
use crate::rooms::claims::StateName;
use serde::{Serialize, de::DeserializeOwned};

const UNKNOWN: [&str; 2] = ["held-by-an-older-build", ""];

fn names<T>(known: &[T], name: fn(&T) -> &str) -> Vec<&str> {
    known.iter().map(name).collect()
}
/// Each known value saves as its name and loads back as itself; unknown text
/// loads as `Unrecognised` and saves back unchanged.
fn pinned<T>(known: &[T], name: fn(&T) -> &str, parse: fn(&str) -> T, saved: &[&str])
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug + std::fmt::Display,
{
    assert_eq!(names(known, name), saved);
    for (status, text) in known.iter().zip(saved) {
        assert_eq!(serde_json::to_string(status).unwrap(), format!("\"{text}\""));
        assert_eq!(&serde_json::from_str::<T>(&format!("\"{text}\"")).unwrap(), status);
        assert_eq!(status.to_string(), *text);
    }
    for text in UNKNOWN {
        let status = serde_json::from_str::<T>(&format!("\"{text}\"")).unwrap();
        assert!(!known.contains(&status));
        assert_eq!(status, parse(text));
        assert_eq!(serde_json::to_string(&status).unwrap(), format!("\"{text}\""));
    }
}
/// A saved record loads and saves back byte for byte for every known status
/// and for unknown text.
fn round_trips<T: Serialize + DeserializeOwned>(known: Vec<&str>, saved: impl Fn(&str) -> String) {
    for status in known.into_iter().chain(UNKNOWN) {
        let text = saved(status);
        let record: T = serde_json::from_str(&text).unwrap();
        assert_eq!(serde_json::to_string(&record).unwrap(), text);
    }
}

#[test]
fn saved_status_strings_are_pinned() {
    pinned(
        JobStatus::KNOWN,
        JobStatus::as_str,
        JobStatus::parse,
        &[
            "preparing",
            "running",
            "stopping",
            "completed",
            "failed",
            "timed_out",
            "cancelled",
            "interrupted",
        ],
    );
    pinned(
        PreviewStatus::KNOWN,
        PreviewStatus::as_str,
        PreviewStatus::parse,
        &["preparing", "running", "failed", "stopped", "unavailable"],
    );
    pinned(
        PublicationStatus::KNOWN,
        PublicationStatus::as_str,
        PublicationStatus::parse,
        &["preparing", "branch_created", "opened"],
    );
    pinned(
        StateName::KNOWN,
        StateName::as_str,
        StateName::parse,
        &["working", "blocked", "done", "abandoned", "released", "unclaimed"],
    );
}

#[test]
fn saved_records_load_and_save_back_byte_for_byte() {
    round_trips::<Job>(names(JobStatus::KNOWN, JobStatus::as_str), |status| {
        format!(
            r#"{{"id":"job","generation":"job-attempt-1","image":"sha256:{}","node":"synthetic-node","command":"true","read_only":false,"build_environment":true,"review_source":false,"deadline_unix":0,"container_id":"synthetic-container","status":"{status}","output":"x","exit_code":0}}"#,
            "a".repeat(64)
        )
    });
    let attempt = serde_json::to_string(&crate::usage::Attempt::default()).unwrap();
    round_trips::<Preview>(names(PreviewStatus::KNOWN, PreviewStatus::as_str), |status| {
        format!(
            r#"{{"id":"preview","chat":"chat","attempt":{attempt},"port":8080,"command_hash":"hash","container_id":null,"url":"http://preview.localhost:1234/","status":"{status}"}}"#
        )
    });
    round_trips::<Publication>(names(PublicationStatus::KNOWN, PublicationStatus::as_str), |status| {
        format!(
            r#"{{"run":"r-fixture","artifact":"a-fixture","repository":"example/project","branch":"control/r-fixture","base":"main","base_commit":"{}","tree":null,"commit":null,"url":null,"status":"{status}","title":"Reviewable change","body":"Synthetic verification"}}"#,
            "1".repeat(40)
        )
    });
    round_trips::<crate::bots::lifecycle::Blocked>(names(StateName::KNOWN, StateName::as_str), |status| {
        format!(
            r#"{{"room":"general","on":"base","producer":"chat-a","status":"{status}","waiting":[{{"key":"walls","chat":"chat-b"}}]}}"#
        )
    });
    round_trips::<crate::bots::lifecycle::Bottleneck>(names(StateName::KNOWN, StateName::as_str), |status| {
        format!(
            r#"{{"room":"general","key":"base","owner":"chat-a","status":"{status}","blocked_on":"stone","open":4,"idle":2,"members":["chat-b"],"waiting":[{{"key":"walls","chat":"chat-b"}}],"dependents":1}}"#
        )
    });
}

#[test]
fn status_predicates_keep_their_sets() {
    let active = JobStatus::KNOWN.iter().filter(|s| s.is_active()).map(JobStatus::as_str);
    assert_eq!(active.collect::<Vec<_>>(), ["preparing", "running", "stopping"]);
    let retained = PreviewStatus::KNOWN
        .iter()
        .filter(|s| s.is_retained())
        .map(PreviewStatus::as_str);
    assert_eq!(
        retained.collect::<Vec<_>>(),
        ["preparing", "running", "failed", "unavailable"]
    );
    // Unknown saved text was never in the active list, and was never "stopped".
    assert!(!JobStatus::parse("restoring").is_active());
    assert!(PreviewStatus::parse("restoring").is_retained());
}

#[test]
fn preview_links_show_the_saved_status_text() {
    for status in PreviewStatus::KNOWN
        .iter()
        .cloned()
        .chain([PreviewStatus::parse("restoring")])
    {
        let text = format!(
            r#"{{"id":"preview","chat":"chat","attempt":{{}},"port":8080,"command_hash":"hash","container_id":null,"url":"http://preview.localhost:1234/","status":"{status}"}}"#
        );
        let preview: Preview = serde_json::from_str(&text).unwrap();
        let html = crate::previews::links(&[preview], "chat");
        let expected = match status {
            PreviewStatus::Stopped => String::new(),
            PreviewStatus::Running => "Open preview ↗".into(),
            PreviewStatus::Preparing
            | PreviewStatus::Failed
            | PreviewStatus::Unavailable
            | PreviewStatus::Unrecognised(_) => format!("<span>Preview · {status}</span>"),
        };
        assert!(html.contains(&expected), "{status}: {html}");
        assert_eq!(html.is_empty(), status == PreviewStatus::Stopped);
    }
}
