#[test]
fn stamps_never_repeat_across_threads() {
    let threads: Vec<_> = (0..8)
        .map(|_| std::thread::spawn(|| (0..2000).map(|_| super::stamp()).collect::<Vec<_>>()))
        .collect();
    let all: Vec<u64> = threads.into_iter().flat_map(|t| t.join().unwrap()).collect();
    let unique: std::collections::HashSet<_> = all.iter().collect();
    assert_eq!(unique.len(), all.len());
}

/// Chat and run ids were unique only within one `State` (`c-<µs>-<count>`), so
/// two tests creating their first chat in the same microsecond shared an import
/// registry entry and a history journal.
#[test]
fn first_chats_and_runs_of_separate_states_differ() {
    let ids: Vec<_> = (0..200)
        .flat_map(|_| {
            let mut state = crate::State::default();
            let chat = crate::chat::create(&mut state);
            state
                .runtime
                .enqueue("objective".into(), crate::runtime::RunKind::Task, "test");
            [chat, state.runtime.runs[0].id().to_string()]
        })
        .collect();
    let unique: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len());
}
