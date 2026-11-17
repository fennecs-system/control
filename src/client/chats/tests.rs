use super::*;
use serde_json::json;

#[test]
fn navigation_is_the_same_vocabulary_as_the_tui() {
    assert_eq!(navigation("/chats"), Some(Navigation::List));
    assert_eq!(navigation(" /resume "), Some(Navigation::List));
    assert_eq!(navigation("/resume c-lead"), Some(Navigation::Open("c-lead".into())));
    assert_eq!(navigation("/new"), Some(Navigation::New));
    assert_eq!(navigation("/resume a b"), None);
    assert_eq!(navigation("/steer go left"), None);
    assert_eq!(navigation("resume c-1"), None);
    assert_eq!(navigation("/rename  Farm lead "), Some(Navigation::Rename("Farm lead".into())));
    assert_eq!(navigation("/rename"), Some(Navigation::Rename(String::new())), "the server answers with usage");
    assert_eq!(navigation("/renamed x"), None);
}

#[test]
fn recent_chats_sort_by_activity_and_list_their_resume_commands() {
    let list = json!([
        {"id":"c-old","title":"Old","depth":0,"activity_order":1,"revision":3},
        {"id":"c-lead","title":"Swarm lead","depth":0,"activity_order":9,"revision":7},
        {"id":"c-helper","title":"Helper","depth":1,"activity_order":5}
    ]);
    let entries = entries(list.as_array().unwrap());
    assert_eq!(entries[2].depth, 1);
    assert_eq!(entries[1].revision, &json!(7));
    let recent = recent(&entries, 2);
    assert_eq!(recent.iter().map(|e| e.id).collect::<Vec<_>>(), ["c-lead", "c-helper"]);
    assert_eq!(resume_list(&recent), "Swarm lead\n  /resume c-lead\n\nHelper\n  /resume c-helper");
    assert!(super::entries(&[]).is_empty());
}

#[test]
fn finished_replies_are_delivered_in_order_once() {
    let detail = json!({"messages":[
        {"status":"completed","reply":"first"},
        {"status":"continued","reply":"paused"},
        {"status":"failed","reply":"second"},
        {"status":"running","reply":""},
        {"status":"completed","reply":"later"}
    ]});
    let (replies, next) = replies(&detail, 0);
    assert_eq!(replies.iter().map(|r| (r.index, r.text)).collect::<Vec<_>>(), [(0, "first"), (2, "second")]);
    assert_eq!(next, 3, "stops at the running message so later replies keep their order");
    let (again, same) = super::replies(&detail, next);
    assert!(again.is_empty() && same == 3);
    let waiting = json!({"messages":[{"status":"waiting","reply":"question"}]});
    assert_eq!(super::replies(&waiting, 0).1, 0, "a waiting question is an operator request, not a reply");
    assert_eq!(super::replies(&detail, 99).1, 5, "a shorter history clamps the cursor");
}

#[test]
fn use_takes_the_tuis_lens_list() {
    assert_eq!(navigation("/use engineering evidence"), Some(Navigation::Use(Some(vec!["engineering".into(), "evidence".into()]))));
    assert_eq!(navigation("/use a,b , c"), Some(Navigation::Use(Some(vec!["a".into(), "b".into(), "c".into()]))));
    assert_eq!(navigation("/use none"), Some(Navigation::Use(Some(vec![]))));
    assert_eq!(navigation("/use  "), Some(Navigation::Use(None)));
    assert_eq!(lenses(""), None);
}

#[test]
fn the_shared_use_grammar_parses_lists_clears_and_ignores_empty_selections() {
    assert_eq!(lenses(" alpha"), Some(vec!["alpha"]));
    assert_eq!(lenses(" alpha beta  gamma"), Some(vec!["alpha", "beta", "gamma"]));
    assert_eq!(lenses(" alpha, beta ,gamma,"), Some(vec!["alpha", "beta", "gamma"]));
    assert_eq!(lenses(" none"), Some(Vec::new()));
    assert_eq!(lenses(" none alpha"), Some(vec!["none", "alpha"]));
    assert_eq!(lenses(" , "), None, "no names: a no-op, not a clear");
    assert_eq!(use_arguments("/use"), Some(""));
    assert_eq!(use_arguments("/user alpha"), None);
    assert_eq!(use_arguments("use alpha"), None);
    // Every empty form is the same no-op on every client.
    for text in ["/use", "/use ", "/use ,", "/use , ,"] {
        assert_eq!(navigation(text), Some(Navigation::Use(None)), "{text:?}");
    }
    assert_eq!(navigation("/use none"), Some(Navigation::Use(Some(vec![]))));
    let library = |id: &str| matches!(id, "alpha" | "beta" | "gamma");
    assert_eq!(unknown(&lenses(" zeta alpha typo, zeta").unwrap(), library), vec!["zeta", "typo"]);
    assert!(unknown(&lenses(" alpha,beta").unwrap(), library).is_empty());
}

#[test]
fn genes_lists_the_library_and_lenses_stays_an_alias() {
    assert_eq!(navigation("/genes"), Some(Navigation::Genes));
    assert_eq!(navigation(" /lenses "), Some(Navigation::Genes));
    assert_eq!(navigation("/genes swarm"), None);
    let list = gene_list(&json!([
        {"id":"swarm","name":"Swarm","revision":3,"includes":["planning","autonomy"]},
        {"id":"evidence","name":"Evidence","revision":1,"includes":[]},
    ]));
    assert!(list.starts_with("Genes. Set a DM's DNA with /use"));
    assert!(list.contains("swarm · Swarm · v3\n  includes planning + autonomy\nevidence · Evidence · v1"));
    assert!(COMMANDS.iter().any(|(usage, _)| *usage == "/genes"));
}
