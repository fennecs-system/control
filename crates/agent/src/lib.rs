//! The interface between Control and an agent backend. A backend is one way to
//! run an agent turn; it supplies [`Agent`] and receives from the host only
//! typed values and the small traits in [`host`]: never application state.
//! See docs/CRATE-BOUNDARIES.md.
pub mod child;
pub mod cli;
mod failure;
mod granted;
pub mod host;
mod key;
pub mod mailbox_wake;

pub use failure::{
    CANCELLED, Cancelled, Cause, Failure, SignIn, StatedRetry, Status, UNFINISHED_TOOL, WAITING, classify,
    stated_retry, timeout_message,
};
pub use granted::{Folder, Form, Granted, Keep, Keeps, KeyDelivery, Names, Proxied, Write, sweep, sweep_plan};
pub use host::{Control, Events, Host, Node, Stop};
pub use key::{ApiKey, KeyError};

use control_isolation::{Isolation, RunSpec};
use std::{
    io::{self, Read},
    path::Path,
    time::Duration,
};

/// How long an offline isolation probe may take. Its container usually exits
/// within seconds, but a busy engine (just after a build, image load or
/// restart) can take far longer to start one.
pub const SANDBOX_PROBE_WAIT: Duration = Duration::from_secs(90);

/// What an agent backend supplies to the host. The facts below are also the
/// list of where backends are treated differently today. Each default is the
/// shared behaviour and each override is a named difference to even out
/// deliberately, not a place to add more.
pub trait Agent: Sync {
    /// The settings key and the `ID:` model prefix.
    fn id(&self) -> &'static str;
    fn label(&self) -> &'static str;
    /// The short name on the status page and in failures.
    fn name(&self) -> &'static str;
    fn default_model(&self) -> &'static str;
    /// Difference: an environment variable may override the default model (Codex).
    fn model_variable(&self) -> Option<&'static str> {
        None
    }
    /// Whether it can be the saved node default. Local endpoints are chosen
    /// per chat or spawn from the endpoint registry.
    fn default_capable(&self) -> bool {
        true
    }
    /// Where the operator pins the image for the attempt's own offline tools.
    fn image(&self) -> ImagePin;
    /// What the operator does when the image is not pinned.
    fn image_hint(&self) -> &'static str;
    /// How its login is checked, or `None` when it has none.
    fn login(&self) -> Option<Login>;
    /// Offline isolation probe with a canary credential: no account, no model.
    fn probe(&self, node: &dyn Node, isolation: &dyn Isolation, image: &str) -> Result<(), String>;
    /// A bounded text request without tools, or why this backend has no CLI.
    fn text_run(&self, node: &dyn Node, isolation: &dyn Isolation, call: &TextCall<'_>) -> io::Result<RunSpec>;
    /// Decodes a text request's event stream; `max` bounds the reply in bytes.
    fn text(
        &self,
        reader: &mut dyn Read,
        max: usize,
        emit: &mut dyn FnMut(control_ai::Event),
    ) -> io::Result<control_ai::Output>;
    /// Who runs the tool loop of a turn.
    fn runs(&self) -> Runs<'_>;
    /// Names it keeps in a workspace that are never published or shared.
    fn private_names(&self) -> &'static [&'static str] {
        &[]
    }
    /// Difference: the host offers its own shell tool because the CLI's is
    /// disabled (Claude, Grok, Antigravity) or absent (local). Codex runs its
    /// own sandboxed shell.
    fn host_shell(&self) -> bool {
        false
    }
    /// Difference: a judge reads its source at `/review` beside a writable
    /// scratch `/workspace` (Codex); others get `/workspace` read-only.
    fn review_mount(&self) -> bool {
        false
    }
    /// Difference: the main container mounts a mailbox directory beside the
    /// workspace instead of the workspace itself.
    fn mailbox(&self) -> Option<&'static str> {
        None
    }
    /// Where the pre-run probe container's workspace is, beside the workspace.
    fn probe_workspace(&self) -> &'static str {
        "sandbox-probe/workspace"
    }
    /// Difference: its own shorter prompt for `objective` (Claude). Others
    /// get Control's shared framing.
    fn own_prompt(&self, _objective: &str, _review_only: bool) -> Option<String> {
        None
    }
    /// Whether the backend keeps its own session memory or controls between
    /// turns. None does yet: every CLI runs ephemeral and each turn starts
    /// from Control's captured context, so nothing reads this.
    fn own_memory(&self) -> bool {
        false
    }
    /// Difference: the CLI keeps its own built-in web search in a turn (Codex;
    /// its text calls turn it off). Others have their own tools removed and
    /// search through Control's tool.
    fn own_web_search(&self) -> bool {
        false
    }
    /// How an API-key account's key reaches the CLI. The shared behaviour is
    /// a mount; a backend whose provider has a relay rule declares
    /// [`KeyDelivery::Proxied`] and handles [`Form::Proxied`].
    fn key_delivery(&self) -> KeyDelivery {
        KeyDelivery::Mounted("its provider has no key-relay rule")
    }
}

/// A pinned image's configuration file, and the variable that overrides it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImagePin {
    pub file: &'static str,
    pub variable: Option<&'static str>,
}
/// The Codex worker image, also the local backend's tool image.
pub const WORKER_IMAGE: ImagePin = ImagePin {
    file: "worker-image-id",
    variable: Some("CONTROL_WORKER_IMAGE"),
};

/// When a turn's login is checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Checked {
    /// Before the workspace is restored, so a missing login leaves nothing behind.
    BeforeWorkspace,
    /// Inside the turn, which words its own refusal.
    InTurn,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Login {
    pub checked: Checked,
    /// The operator's next step when the login is missing; `None` shows
    /// what the host found instead.
    pub hint: Option<&'static str>,
}

/// Difference: who runs a turn's tool loop.
pub enum Runs<'a> {
    /// The backend's own CLI, in a container the backend describes.
    Cli(&'a dyn Turns),
    /// Control's own loop on a local endpoint.
    Control,
}
/// A backend whose CLI runs a whole turn.
pub trait Turns: Sync {
    fn turn(&self, turn: Turn<'_>, host: &mut dyn Host) -> Outcome;
}

/// One bounded text request's container identity.
pub struct TextCall<'a> {
    pub image: &'a str,
    pub name: &'a str,
    pub workspace: &'a Path,
    pub node: &'a str,
    pub model: &'a str,
    /// The backend's login, when it has one.
    pub login: Option<&'a Granted>,
}
impl TextCall<'_> {
    /// The login of a backend that has one.
    pub fn granted(&self) -> io::Result<&Granted> {
        self.login
            .ok_or_else(|| io::Error::other("No login was granted for this text call"))
    }
}

/// What one CLI turn receives from the host.
pub struct Turn<'a> {
    /// The attempt's workspace identity; container names derive from it.
    pub id: &'a str,
    pub isolation: &'a dyn Isolation,
    pub node: &'a str,
    pub image: &'a str,
    pub model: &'a str,
    /// The prepared workspace, canonical; its parent holds the attempt's
    /// private files.
    pub workspace: &'a Path,
    /// The rendered prompt; [`Host::observed`] adds the observation.
    pub prompt: String,
    /// A judge's turn: its source is read-only.
    pub review_only: bool,
    /// When the attempt was admitted, in Unix seconds.
    pub started_unix: u64,
    /// Control's tools are served to this turn (a live node).
    pub tools: bool,
    /// The login the host located for this turn.
    pub login: io::Result<Granted>,
}

/// How a turn ended.
#[derive(Debug)]
pub enum Outcome {
    /// The provider never started.
    NotLaunched(Failure),
    /// The provider started; its usage when it was read.
    Launched {
        result: Result<String, Failure>,
        usage: Option<control_ai::Usage>,
    },
}
impl From<Failure> for Outcome {
    fn from(failure: Failure) -> Self {
        Self::NotLaunched(failure)
    }
}

/// Seconds since the attempt's admission at the moment a loop starts.
pub fn admitted(started_unix: u64) -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .saturating_sub(started_unix)
}

/// A backend's operator subcommand (`control ID login|check`), with what the
/// host located for it.
pub struct Command<'a> {
    pub args: &'a [String],
    pub node: &'a dyn Node,
    pub isolation: &'a dyn Isolation,
    pub image: Option<String>,
    /// The account's login directory, which `login` creates.
    pub directory: std::path::PathBuf,
    /// The account's login, as a turn would be granted it.
    pub login: io::Result<Granted>,
    /// The key read for `key`, handed only to a backend whose CLI writes its
    /// own sign-in from a key (Codex); the host saves every other key itself.
    pub key: Option<&'a ApiKey>,
}
