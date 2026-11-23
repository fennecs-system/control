//! Closed backend selection, captured with each run and attempt.
use serde::{Deserialize, Serialize};
use std::{io::{self, Read}, path::Path};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    #[default]
    Codex,
    Claude,
    Grok,
    Antigravity,
    /// An operator-configured private inference endpoint, named by its text
    /// model ID. Text generation is available; agent turns are not yet.
    Local,
}
/// What an agent backend supplies to the host. Signatures name only `std`,
/// `control_ai` and `control_isolation`, so an implementation can move to its own crate; what a
/// backend needs from the host arrives as arguments.
///
/// The facts below are also the list of where backends are treated differently
/// today. Each default is the shared behaviour and each override is a named
/// difference to even out deliberately, not a place to add more.
pub(crate) trait Agent: Sync {
    /// The settings key and the `ID:` model prefix.
    fn id(&self) -> &'static str;
    fn label(&self) -> &'static str;
    /// The short name on the status page and in text-call failures.
    fn name(&self) -> &'static str;
    fn default_model(&self) -> String;
    /// Whether it can be the saved node default. Local endpoints are chosen
    /// per chat or spawn from the endpoint registry.
    fn default_capable(&self) -> bool {
        true
    }
    /// The pinned image for the attempt's own offline tools.
    fn image(&self) -> Option<String>;
    /// What the operator does when the image is not pinned.
    fn image_hint(&self) -> &'static str;
    /// Credential metadata only; the error is the operator's next step.
    fn credentials_ready(&self) -> Result<(), String>;
    /// Offline isolation probe with a canary credential: no account, no model.
    fn probe(&self, isolation: &dyn control_isolation::Isolation, image: &str) -> Result<(), String>;
    /// A bounded text request without tools, or why this backend has no CLI.
    fn text_run(&self, isolation: &dyn control_isolation::Isolation, call: &TextCall<'_>) -> io::Result<control_isolation::RunSpec>;
    /// Decodes a text request's event stream; `max` bounds the reply in bytes.
    fn text(&self, reader: &mut dyn Read, max: usize, emit: &mut dyn FnMut(control_ai::Event)) -> io::Result<control_ai::Output>;
    /// Names it keeps in a workspace that are never published or shared.
    fn private_names(&self) -> &'static [&'static str] {
        &[]
    }
    /// Difference: the host offers its own shell tool because the CLI's is
    /// disabled (Claude) or absent (local). Codex runs its own sandboxed shell.
    fn host_shell(&self) -> bool {
        false
    }
    /// Difference: a judge reads its source at `/review` beside a writable
    /// scratch `/workspace` (Codex); others get `/workspace` read-only.
    fn review_mount(&self) -> bool {
        false
    }
    /// Difference: the main container mounts a mailbox directory beside the
    /// workspace instead of the workspace itself (Claude).
    fn mailbox(&self) -> Option<&'static str> {
        None
    }
    /// Where the pre-run probe container's workspace is, beside the workspace.
    fn probe_workspace(&self) -> &'static str {
        "sandbox-probe/workspace"
    }
    /// Difference: it renders its own shorter prompt and takes no captured
    /// turn context (Claude). Others get Control's shared framing.
    fn own_prompt(&self) -> bool {
        false
    }
    /// Whether the backend keeps its own session memory or controls between
    /// turns. None does yet: every CLI runs ephemeral and each
    /// turn starts from Control's captured context, so nothing reads this.
    #[allow(dead_code)]
    fn own_memory(&self) -> bool {
        false
    }
}
/// One bounded text request's container identity.
pub(crate) struct TextCall<'a> {
    pub image: &'a str,
    pub name: &'a str,
    pub workspace: &'a Path,
    pub node: &'a str,
    pub model: &'a str,
}
impl Backend {
    pub(crate) const ALL: [Self; 5] = [Self::Codex, Self::Claude, Self::Grok, Self::Antigravity, Self::Local];
    /// The backends that run a pinned CLI image and can be the node default.
    pub(crate) fn hosted() -> impl Iterator<Item = Self> {
        Self::ALL.into_iter().filter(|backend| backend.default_capable())
    }
    pub(crate) fn agent(self) -> &'static dyn Agent {
        match self {
            Self::Codex => &crate::codex::Codex,
            Self::Claude => &crate::claude::Claude,
            Self::Grok => &crate::grok::Grok,
            Self::Antigravity => &crate::antigravity::Antigravity,
            Self::Local => &crate::local_agent::Local,
        }
    }
    pub(crate) fn named(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|backend| backend.id() == id)
    }
    pub fn label(self) -> &'static str {
        self.agent().label()
    }
    pub fn id(self) -> &'static str {
        self.agent().id()
    }
    pub fn default_model(self) -> String {
        self.agent().default_model()
    }
    pub fn default_capable(self) -> bool {
        self.agent().default_capable()
    }
    pub fn image(self) -> Option<String> {
        self.agent().image()
    }
}
/// Whether any backend keeps `name` private in a workspace.
pub(crate) fn private_name(name: &str) -> bool {
    Backend::ALL.iter().any(|backend| backend.agent().private_names().contains(&name))
}
/// Refusal shared by selection and launch for endpoints without declared tool calls.
pub const LOCAL_TEXT_ONLY: &str = "This local endpoint is declared text-only: use control_model_generate for inference, or edit the entry in Manage → Models and enable \"Serves tool calls\" for a tool-capable server (llama.cpp --jinja) to run agent turns. No run was started and no hosted model was substituted.";
