//! The operator's form posts that apply under the state lock and are saved
//! after it is released (`Store::commit`): `/task`, `/retry-run`, `/room` and
//! `/spawn`. The caller is redirected only once its change is durable. If
//! storage fails after the release, others may already see the change, so it
//! stays and is saved when storage recovers: the caller gets a 503 page that
//! says it is not saved yet. If storage refused it before the release, it was
//! never applied or was undone: a 500 page that says it was not saved. The
//! other form posts still save under the lock (`state_response`).
use super::*;
use crate::application::{effects::Effects, runs::Tx};
use crate::store::{Applied, Commit};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Post {
    Task,
    Retry,
    Room,
    Spawn,
}
impl Post {
    pub(super) fn of(method: &str, path: &str) -> Option<Self> {
        match (method, path) {
            ("POST", "/task") => Some(Self::Task),
            ("POST", "/retry-run") => Some(Self::Retry),
            ("POST", "/room") => Some(Self::Room),
            ("POST", "/spawn") => Some(Self::Spawn),
            _ => None,
        }
    }
    /// Where the browser goes afterwards; it does not depend on the change.
    fn location(self, f: &HashMap<String, String>) -> String {
        match self {
            Self::Task | Self::Retry => history_view::AFTER_ACTION.to_owned(),
            Self::Room => f.get("channel").filter(|room| rooms::valid(room)).map_or_else(
                || "/?view=rooms".to_owned(),
                |room| format!("/?view=rooms&channel={room}"),
            ),
            Self::Spawn => "/?view=agents".to_owned(),
        }
    }
    /// Applies the post. Refusals (a full queue, an empty objective, an
    /// invalid room) become notices or no change, as before.
    fn apply(self, state: &mut State, f: &HashMap<String, String>, tx: Tx) -> Effects {
        match self {
            Self::Task => state.runtime.post_task(f, tx),
            Self::Retry => {
                let _ = state.runtime.action("/retry-run", f);
                Effects::none()
            }
            Self::Room => {
                let _ = state.collective.action("/room", f);
                Effects::none()
            }
            Self::Spawn => {
                if let Some(task) = f.get("objective").filter(|s| !s.trim().is_empty()) {
                    state.agents.push(
                        format!(
                            "{} · {}",
                            task.chars().take(300).collect::<String>(),
                            f.get("provider").unwrap_or(&"Self-hosted".into())
                        )
                        .into(),
                    );
                }
                Effects::none()
            }
        }
    }
}

/// What a post may change, to undo it while the lock is still held.
struct Before {
    runs: usize,
    notice: String,
    /// The retried run's status, queue position and result.
    retried: Option<(String, runtime::RunStatus, u64, String)>,
    posts: usize,
    room_notice: String,
    agents: usize,
}
impl Before {
    fn of(state: &State, post: Post, f: &HashMap<String, String>) -> Self {
        Self {
            runs: state.runtime.runs.len(),
            notice: state.runtime.notice.clone(),
            retried: (post == Post::Retry)
                .then(|| f.get("id").map(|id| id.trim()))
                .flatten()
                .and_then(|id| state.runtime.runs.get(id))
                .map(|run| {
                    (
                        run.id().as_str().to_owned(),
                        run.status.clone(),
                        run.queue_order,
                        run.result.to_string(),
                    )
                }),
            posts: state.collective.posts.len(),
            room_notice: (*state.collective.notice).clone(),
            agents: state.agents.len(),
        }
    }
    fn restore(self, state: &mut State) {
        state.runtime.runs.truncate(self.runs);
        if let Some((id, status, order, result)) = self.retried
            && let Some(run) = state.runtime.runs.get_mut(&id)
        {
            run.status = status;
            run.queue_order = order;
            *run.result = result;
        }
        state.runtime.notice = self.notice;
        state.collective.posts.truncate(self.posts);
        *state.collective.notice = self.room_notice;
        state.agents.truncate(self.agents);
    }
}

/// Runs `post` through the store's driver and answers the browser.
pub(super) fn respond(stream: &mut impl Write, store: &Store, post: Post, body: &[u8]) {
    let f = fields(&String::from_utf8_lossy(body));
    let location = post.location(&f);
    let committed = commit(store, post, &f, crate::storage::submit, crate::worker::wake);
    let mut response = Vec::new();
    let page = |response: &mut Vec<u8>, status: &str, message: &str| {
        let body = error_page(message, &location);
        let _ = control_http::respond(
            response,
            status,
            "text/html; charset=utf-8",
            body.as_bytes(),
            "",
            control_http::Policy::Page,
        );
    };
    match committed {
        Ok(Commit::Saved(())) => {
            let _ = control_http::respond(
                &mut response,
                "303 See Other",
                "text/plain",
                b"",
                &format!("Location: {location}\r\n"),
                control_http::Policy::Page,
            );
        }
        Ok(Commit::NotYet((), unsaved)) => page(&mut response, "503 Service Unavailable", &unsaved.to_string()),
        Ok(Commit::NotSaved(error)) => page(
            &mut response,
            "500 Internal Server Error",
            &format!("Could not persist the local change: {error}"),
        ),
        Ok(Commit::Refused(never, _)) => match never {},
        Err(_) => unavailable(&mut response),
    }
    let _ = stream.write_all(&response);
}

/// The post as a transaction, with the stage and the wake passed in (tests
/// script the disk).
pub(super) fn commit(
    store: &Store,
    post: Post,
    f: &HashMap<String, String>,
    submit: impl FnOnce(&State, crate::store::Staging) -> std::io::Result<crate::storage::Saving>,
    wake: impl FnOnce(),
) -> Result<Commit<(), std::convert::Infallible>, crate::store::Unavailable> {
    store.commit_with(
        |state, tx| {
            let before = Before::of(state, post, f);
            let effects = post.apply(state, f, tx);
            Ok(Applied {
                answer: (),
                effects,
                undo: move |state: &mut State| before.restore(state),
            })
        },
        submit,
        wake,
    )
}
