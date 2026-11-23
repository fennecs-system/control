//! Google's Antigravity CLI (`agy`), run like Grok Build: its own tools are
//! denied, its only tools are Control's over MCP, and executable work runs in
//! separate offline containers through the host shell tool.
use control_agent::{
    Agent, Cause, Checked, Command, Failure, Form, Granted, Host, ImagePin, Keeps, KeyDelivery, Login, Node, Outcome,
    Runs, TextCall, Turn, Turns,
    cli::{self, Canary, Cli, Delivery, Ending, Probe},
};
use control_isolation::{Access, Input, Isolation, RunSpec, Size, Stream, Streams, Tmpfs, User};
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, Read},
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

pub struct Antigravity;
impl Agent for Antigravity {
    fn id(&self) -> &'static str {
        "antigravity"
    }
    fn label(&self) -> &'static str {
        "Google / Antigravity CLI"
    }
    /// Its credential names: never mounted in a workspace today, and never published if one appears there.
    fn key_delivery(&self) -> KeyDelivery {
        KeyDelivery::Proxied(&control_inject::GEMINI)
    }
    fn private_names(&self) -> &'static [&'static str] {
        &[".gemini", "gemini-api-key", "adc.json", "antigravity-oauth-token"]
    }
    fn name(&self) -> &'static str {
        "Antigravity"
    }
    fn default_model(&self) -> &'static str {
        "gemini-4-neon-medium"
    }
    fn image(&self) -> ImagePin {
        ImagePin {
            file: "antigravity-image-id",
            variable: None,
        }
    }
    fn image_hint(&self) -> &'static str {
        "run nix run .#setup -- --antigravity, or set antigravity-image-id in the configuration directory to the Nix image digest"
    }
    fn login(&self) -> Option<Login> {
        Some(Login {
            checked: Checked::InTurn,
            hint: Some(LOGIN),
        })
    }
    fn probe(&self, node: &dyn Node, isolation: &dyn Isolation, image: &str) -> Result<(), String> {
        cli::check(node, isolation, image, self.id(), self.name(), &PROBE)
    }
    fn text_run(&self, _: &dyn Node, _: &dyn Isolation, call: &TextCall<'_>) -> io::Result<RunSpec> {
        let login = call.granted()?;
        // The text call's scratch root is private and removed after a success.
        let root = call
            .workspace
            .parent()
            .ok_or_else(|| fail("Invalid text-call workspace"))?;
        let private = prepare(root, login, None, None)?;
        text_container(call, login, &private)
    }
    fn text(
        &self,
        reader: &mut dyn Read,
        _max: usize,
        emit: &mut dyn FnMut(control_ai::Event),
    ) -> io::Result<control_ai::Output> {
        control_ai::json_lines(reader, control_ai::antigravity::Antigravity::text(), emit)
    }
    fn runs(&self) -> Runs<'_> {
        Runs::Cli(self)
    }
    fn host_shell(&self) -> bool {
        true
    }
    fn mailbox(&self) -> Option<&'static str> {
        Some(MAILBOX)
    }
    fn probe_workspace(&self) -> &'static str {
        PROBE.directory
    }
}
impl Turns for Antigravity {
    /// The prompt is the same captured turn context or framing Codex and
    /// local models receive.
    fn turn(&self, turn: Turn<'_>, host: &mut dyn Host) -> Outcome {
        let model = turn.model.to_owned();
        let (node, image) = (turn.node.to_owned(), turn.image.to_owned());
        cli::turn(&CLI, turn, host, |prepared| {
            let blocked = |message: &str| Failure::Blocked(message.to_owned());
            let private = prepare(
                prepared.root,
                prepared.login,
                Some(&mcp_config(prepared.login.form())),
                prepared.prompt.as_deref(),
            )
            .map_err(|_| Failure::Failed("Cannot prepare Antigravity configuration".into()))?;
            container(prepared.name, &node, prepared.login, &private, &image)
                .map_err(|_| blocked("Antigravity login metadata or mount path unavailable"))?;
            cli::checked(prepared.mailbox).map_err(|_| blocked("Invalid Antigravity mount path"))?;
            turn_container(
                prepared.name,
                &node,
                prepared.login,
                &private,
                prepared.mailbox,
                &image,
                &model,
            )
            .map_err(|_| blocked("Antigravity login metadata or mount path unavailable"))
        })
    }
}
const MAILBOX: &str = "antigravity-mailbox";
/// Control's files for one container, beside the workspace and never in it.
const PRIVATE: &str = "antigravity-private";
const LOGIN: &str = "run control antigravity login, or save a Gemini API key; the login stays outside the checkout";
const PROBE: Probe = Probe {
    directory: "antigravity-isolation-probe",
    command: "set -eu; test ! -e /home/agent/.gemini; test ! -e /home/agent/.control-key; test ! -e /home/agent/.config/gcloud; test ! -S /var/run/docker.sock; test ! -e /proc/1/root/home/agent/.gemini; test -z \"${GEMINI_API_KEY-}${AGY_ADC_AUTH-}\"; printf verified > isolation-check",
    timed_out: "Antigravity tool isolation probe timed out after 90 s: the container engine is busy or slow, which is not an isolation failure. No account credentials were mounted; retry shortly.",
    failed: "Antigravity workspace isolation check failed; no credentials were mounted",
};
const CLI: Cli = Cli {
    name: "Antigravity",
    probe: &PROBE,
    mailbox: MAILBOX,
    no_login: "Antigravity login unavailable. Run control antigravity login or save a Gemini API key; no other login on this host is used.",
    delivery: Delivery::File,
    keeps: KEEPS,
    capture: |reader, update, progress, bad_tools| {
        cli::capture_with(
            reader,
            control_ai::antigravity::Antigravity::agent(),
            "Antigravity",
            update,
            progress,
            bad_tools,
        )
    },
    failure: diagnose,
};
/// Headless and unattended, without `--dangerously-skip-permissions`: headless
/// mode cannot prompt, so an action no rule allows is refused
/// (https://antigravity.google/docs/cli/headless) rather than approved, and the
/// deny list in [`settings`] need not name every future action. Checked
/// offline against a scripted model endpoint: Control's MCP tools run under
/// the `mcp(control/*)` allow rule and every denied read is refused.
const FLAGS: [&str; 3] = ["--disable-slash-commands", "--output-format", "stream-json"];
/// Every built-in action class the CLI's rule parser accepts besides `mcp`
/// (its pattern in 1.5.14:
/// `^(command|read_file|write_file|read_url|mcp|execute_url|download_url|unsandboxed)\s*\(.*\)$`).
/// It ignores a rule it cannot parse (https://antigravity.google/docs/settings).
const DENY: [&str; 7] = [
    "command(*)",
    "unsandboxed(*)",
    "read_file(*)",
    "write_file(*)",
    "read_url(*)",
    "execute_url(*)",
    "download_url(*)",
];
/// Where the sign-in token, API key and ADC files live inside the container.
/// In a container the CLI keeps a Google sign-in in this file rather than the
/// OS keyring; its own log says so ("container environment detected").
const TOKEN: &str = "/home/agent/.gemini/antigravity-cli/antigravity-oauth-token";
/// The same file as `control antigravity login` saved it, in the login directory.
pub const SAVED: &str = "login/gemini/antigravity-cli/antigravity-oauth-token";
const KEY: &str = "/home/agent/.control-key/gemini-api-key";
/// How the CLI writes its refreshed Google token back, as far as is known: its
/// token store is closed source (`authstore.fileStore.save` in 1.5.14), and
/// offline it neither refreshed nor rewrote a fake expired token, so the
/// method is unconfirmed. The token file sits in `~/.gemini/antigravity-cli/`
/// with the CLI's caches, skills and per-turn settings, so only the file is
/// mounted: an in-place rewrite is kept; a replacement by rename fails on the
/// mounted file and the refreshed token stays in memory for the turn, as it
/// did read-only (docs/ANTIGRAVITY.md).
const KEEPS: Keeps = Keeps::InPlace;
const ADC: &str = "/home/agent/.config/gcloud/application_default_credentials.json";

fn fail(message: &str) -> io::Error {
    io::Error::other(message)
}
fn private_owned(meta: &fs::Metadata) -> bool {
    meta.mode() & 0o077 == 0 && meta.uid() != 0
}
/// The settings every run gets. Deny rules win over allow rules and apply in
/// every mode (https://antigravity.google/docs/permissions), so the CLI keeps
/// no file, shell or web access; a turn may call only Control's MCP tools.
fn settings(login: Form, tools: bool) -> Value {
    let mut settings = json!({
        "permissions": {"allow": if tools { vec!["mcp(control/*)"] } else { vec![] }, "deny": DENY},
        "artifactReviewPolicy": "always-proceed",
        "enableTerminalSandbox": false,
        "allowNonWorkspaceAccess": false,
        "disableSlashCommands": true,
        "trustedWorkspaces": ["/workspace"],
    });
    if matches!(login, Form::ApiKey | Form::Proxied) {
        settings["modelProvider"] = json!("gemini");
    }
    settings
}
/// Control's tools as the one MCP server. Each is loaded eagerly: a lazy tool
/// is found by reading its schema file, which the deny list forbids
/// (https://antigravity.google/docs/mcp#tool-loading-lazy-vs-eager).
fn mcp_config(login: Form) -> Value {
    let tools: serde_json::Map<String, Value> = control_context::worker_mcp_names()
        .into_iter()
        .map(|name| (name.to_owned(), json!({"eager": true})))
        .collect();
    let server = match login {
        // The entrypoint exports a key for the CLI (tools/antigravity); the
        // CLI does not document it, but a server inherits its environment
        // (seen offline in 1.5.14); Control's server needs no key, so it
        // starts without it.
        Form::ApiKey | Form::Proxied => {
            json!({"command": "/bin/env", "args": ["-u", "GEMINI_API_KEY", "/bin/control-room-mcp"], "tools": tools})
        }
        Form::Session | Form::CloudDefault => json!({"command": "/bin/control-room-mcp", "tools": tools}),
    };
    json!({"mcpServers": {"control": server}})
}
/// Writes the private directory a container reads its settings, MCP server
/// and prompt from.
fn prepare(root: &Path, login: &Granted, mcp: Option<&Value>, prompt: Option<&str>) -> io::Result<PathBuf> {
    let directory = root.join(PRIVATE);
    fs::DirBuilder::new().recursive(true).mode(0o700).create(&directory)?;
    cli::private_file(
        &directory.join("settings.json"),
        &settings(login.form(), mcp.is_some()).to_string(),
    )?;
    if let Some(mcp) = mcp {
        cli::private_file(&directory.join("mcp_config.json"), &mcp.to_string())?;
    }
    if let Some(prompt) = prompt {
        cli::private_file(&directory.join("prompt"), prompt)?;
    }
    Ok(directory)
}
/// The shared CLI container: the CLI's home is a tmpfs, so sessions, logs and
/// its tool cache die with it; the login is mounted for this container only.
fn container(name: &str, node: &str, login: &Granted, private: &Path, image: &str) -> io::Result<RunSpec> {
    let owner = login.owner();
    let spec = RunSpec {
        image: image.into(),
        input: Input::Attached,
        ..cli::base(name, node, owner)
    }
    .tmpfs(Tmpfs::new("/home/agent/.gemini", Size::Mib(256)).owned(owner))
    .bind(cli::checked(private)?, "/home/agent/.control", Access::ReadOnly);
    login_secret(spec, login, private)
}
fn text_container(call: &TextCall<'_>, login: &Granted, private: &Path) -> io::Result<RunSpec> {
    if login.form() == Form::Proxied {
        return Err(fail("A text call never uses a proxied key"));
    }
    Ok(container(call.name, call.node, login, private, call.image)?
        .bind(cli::checked(call.workspace)?, "/workspace", Access::ReadOnly)
        .args(["control-agy", "text"])
        .args(FLAGS)
        .args(["--model", call.model]))
}
fn turn_container(
    name: &str,
    node: &str,
    login: &Granted,
    private: &Path,
    mailbox: &Path,
    image: &str,
    model: &str,
) -> io::Result<RunSpec> {
    let spec = container(name, node, login, private, image)?
        .bind(cli::checked(mailbox)?, "/workspace", Access::ReadWrite)
        .args(["control-agy", "turn"])
        .args(FLAGS)
        .args(["--model", model]);
    match login.form() {
        Form::Proxied => Ok(login.relay()?.offline(spec)),
        Form::ApiKey | Form::Session | Form::CloudDefault => Ok(spec),
    }
}
fn login_container(
    name: &str,
    node: &str,
    owner: User,
    home: &Path,
    config: &Path,
    image: &str,
) -> io::Result<RunSpec> {
    Ok(RunSpec {
        image: image.into(),
        input: Input::Terminal,
        ..cli::base(name, node, owner)
    }
    // The sign-in's own home and configuration, kept: the CLI writes its token there.
    .secret(
        "antigravity/login-home",
        cli::checked(home)?,
        "/home/agent/.gemini",
        Access::ReadWrite,
    )
    .secret(
        "antigravity/login-config",
        cli::checked(config)?,
        "/home/agent/.config/antigravity",
        Access::ReadWrite,
    )
    // No browser can open here. Over SSH the CLI prints an address
    // and reads the code back (https://antigravity.google/docs/install).
    .env("SSH_CONNECTION", "control-login 0 container 22")
    .tmpfs(Tmpfs::new("/workspace", Size::Mib(1)))
    .args(["control-agy", "login"]))
}
/// The credential canary in a turn's own container, for each form of login,
/// on a fake credential.
fn canary(isolation: &dyn Isolation, node: &str, root: &Path, image: &str) -> io::Result<()> {
    let mailbox = root.join(MAILBOX);
    fs::create_dir_all(&mailbox)?;
    let name = format!("control-antigravity-probe-{}-canary", std::process::id());
    [
        (Canary::ReadOnly(Form::ApiKey), KEY),
        (Canary::Refreshable(KEEPS), TOKEN),
        (Canary::ReadOnly(Form::CloudDefault), ADC),
    ]
    .into_iter()
    .try_for_each(|(login, target)| {
        cli::canary_check(isolation, root, login, target, |login| {
            let private = prepare(root, login, Some(&mcp_config(login.form())), Some(""))?;
            turn_container(&name, node, login, &private, &mailbox, image, "canary")
        })
    })?;
    cli::proxied_canary_check(isolation, root, KEY, |login| {
        let private = prepare(root, login, Some(&mcp_config(login.form())), Some(""))?;
        turn_container(&name, node, login, &private, &mailbox, image, "canary")
    })
}
/// Lists models with the saved login, without a workspace.
fn check_container(name: &str, node: &str, login: &Granted, private: &Path, image: &str) -> io::Result<RunSpec> {
    Ok(container(name, node, login, private, image)?
        .tmpfs(Tmpfs::new("/workspace", Size::Mib(1)))
        .args(["control-agy", "check"]))
}
/// The login where the CLI reads it. A proxied key is its placeholder at the
/// key's place, written beside the private settings, which `control-agy`
/// exports as GEMINI_API_KEY; the CLI is pointed at the relay
/// (`GOOGLE_GEMINI_BASE_URL`, with `modelProvider` "gemini").
fn login_secret(spec: RunSpec, login: &Granted, private: &Path) -> io::Result<RunSpec> {
    match login.form() {
        Form::ApiKey => login.mount(spec, "antigravity/api-key", KEY),
        Form::Proxied => {
            let relay = login.relay()?;
            Ok(relay
                .place(spec, &private.join(cli::PLACEHOLDER), relay.placeholder(), KEY)?
                .env("GOOGLE_GEMINI_BASE_URL", &relay.origin()))
        }
        Form::CloudDefault => login.mount(spec.env("AGY_ADC_AUTH", "true"), "antigravity/adc", ADC),
        Form::Session => login.mount_session(spec, "antigravity/google-token", TOKEN, KEEPS),
    }
}
/// A fixed message for a failure the shared classifier knows (a usage limit, a
/// rejected login), after the CLI's own sign-in wording; otherwise its ending.
fn diagnose(ended: Option<Ending>, stderr: &[u8]) -> Failure {
    let own = |bytes: &[u8]| {
        let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
        let cause = if [
            "authentication required",
            "authentication failed",
            "authentication expired",
            "log back in",
            "not logged into",
            "api key not valid",
            "gemini_api_key is not set",
        ]
        .iter()
        .any(|s| text.contains(s))
        {
            Some(Cause::SignIn)
        } else if text.contains("resource_exhausted") || text.contains("out of quota") {
            Some(Cause::UsageLimit)
        } else {
            control_agent::classify(bytes)
        };
        cause.map(|cause| Failure::Provider {
            agent: "Antigravity",
            cause,
        })
    };
    // A rejected tool is never read as a provider cause, whatever its wording.
    ended
        .as_ref()
        .filter(|e| !e.rejected_tools)
        .and_then(|e| own(e.text.as_bytes()))
        .or_else(|| ended.map(Ending::failure))
        .or_else(|| own(stderr))
        .unwrap_or(Failure::Unclassified(
            "Antigravity stopped without success. Check its login, selected model and usage limits.",
        ))
}

/// `control antigravity login | check`.
pub fn command(command: Command<'_>) -> io::Result<()> {
    const USAGE: &str = "Use control antigravity login | check";
    let Command {
        args,
        node,
        isolation,
        image,
        directory,
        login,
        ..
    } = command;
    let [action] = args else { return Err(fail(USAGE)) };
    if matches!(action.as_str(), "--help" | "-h" | "help") {
        println!(
            "control antigravity login: sign in to Google inside the pinned Nix Antigravity container; the token stays in a private directory outside the checkout\ncontrol antigravity key --account NAME: save an API-key account's Gemini key from standard input\ncontrol antigravity check: offline tool isolation, a fake-credential canary in a turn container for each login form, then the binary, the saved login and the model list it can see"
        );
        return Ok(());
    }
    let image = || {
        image
            .clone()
            .ok_or_else(|| fail("Build and pin the Antigravity image first: nix run .#setup -- --antigravity"))
    };
    let node_id = || node.id().map_err(|e| fail(&e));
    match action.as_str() {
        "login" => {
            let (home, config) = (directory.join("login/gemini"), directory.join("login/config"));
            for path in [&home, &config] {
                fs::DirBuilder::new().recursive(true).mode(0o700).create(path)?;
            }
            let meta = fs::symlink_metadata(&directory)?;
            if !meta.is_dir() || !private_owned(&meta) {
                return Err(fail(
                    "Antigravity login directory must be private and owned by a non-root user",
                ));
            }
            // The sign-in's own home, kept: the CLI writes its token there.
            let spec = login_container(
                &format!("control-antigravity-login-{}", std::process::id()),
                &node_id()?,
                User {
                    uid: meta.uid(),
                    gid: meta.gid(),
                },
                &home,
                &config,
                &image()?,
            )?;
            if !isolation.spawn(&spec, Streams::INHERIT)?.wait()?.success() {
                return Err(fail("Antigravity login did not complete"));
            }
            let token = directory.join(SAVED);
            if fs::symlink_metadata(&token).is_ok_and(|m| m.is_file()) {
                fs::set_permissions(&token, fs::Permissions::from_mode(0o600))?;
            }
            Ok(())
        }
        "check" => {
            let image = image()?;
            let node_id = node_id()?;
            let state = node.check_root("antigravity-check").map_err(|e| fail(&e))?;
            cli::probe(
                node,
                isolation,
                &image,
                &node_id,
                &state,
                &format!("control-antigravity-probe-{}", std::process::id()),
                &PROBE,
            )?;
            println!(
                "Antigravity offline workspace isolation verified. No account credentials were mounted or model called."
            );
            canary(isolation, &node_id, &state, &image)?;
            let login = login?;
            let private = prepare(&state, &login, None, None)?;
            let spec = check_container(
                &format!("control-antigravity-probe-{}-login", std::process::id()),
                &node_id,
                &login,
                &private,
                &image,
            )?;
            if !isolation
                .spawn(&spec, Streams::new(Stream::Null, Stream::Inherit, Stream::Inherit))?
                .wait()?
                .success()
            {
                return Err(fail("Antigravity could not list models with the saved login"));
            }
            Ok(())
        }
        _ => Err(fail(USAGE)),
    }
}
#[cfg(test)]
mod tests;
