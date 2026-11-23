//! xAI's Grok Build CLI, run like Claude Code: its own tools are removed, its
//! only tools are Control's over MCP, and executable work runs in separate
//! offline containers through the host shell tool.
use control_agent::{
    Agent, Checked, Command, Failure, Folder, Form, Granted, Host, ImagePin, Keep, Keeps, KeyDelivery, Login, Names,
    Node, Outcome, Runs, TextCall, Turn, Turns,
    cli::{self, Canary, Cli, Delivery, Ending, Probe},
};
use control_isolation::{Access, Input, Isolation, RunSpec, Size, Streams, Tmpfs, User};
use std::{
    fs,
    io::{self, Read},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
};

pub struct Grok;
impl Agent for Grok {
    fn id(&self) -> &'static str {
        "grok"
    }
    fn label(&self) -> &'static str {
        "xAI / Grok Build"
    }
    /// Its credential names: never mounted in a workspace today, and never published if one appears there.
    fn private_names(&self) -> &'static [&'static str] {
        &[".grok", ".grok-login", KEY_FILE]
    }
    /// Implemented to Grok Build's source at the pinned revision; not yet run
    /// against a real Grok image (docs/GROK.md).
    fn key_delivery(&self) -> KeyDelivery {
        KeyDelivery::Proxied(&control_inject::XAI)
    }
    fn name(&self) -> &'static str {
        "Grok"
    }
    fn default_model(&self) -> &'static str {
        "grok-4.7"
    }
    fn image(&self) -> ImagePin {
        ImagePin {
            file: "grok-image-id",
            variable: None,
        }
    }
    fn image_hint(&self) -> &'static str {
        "run nix run .#setup -- --grok, or set grok-image-id in the configuration directory to the Nix image digest"
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
        text_container(call, call.granted()?)
    }
    fn text(
        &self,
        reader: &mut dyn Read,
        _max: usize,
        emit: &mut dyn FnMut(control_ai::Event),
    ) -> io::Result<control_ai::Output> {
        control_ai::json_lines(reader, control_ai::grok::Grok::text(), emit)
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
impl Turns for Grok {
    /// The prompt is the same captured turn context or framing Codex and
    /// local models receive.
    fn turn(&self, turn: Turn<'_>, host: &mut dyn Host) -> Outcome {
        let model = turn.model.to_owned();
        let (node, image) = (turn.node.to_owned(), turn.image.to_owned());
        cli::turn(&CLI, turn, host, |prepared| {
            let blocked = |message: &str| Failure::Blocked(message.to_owned());
            let (config, prompt) = (
                prepared.root.join("grok-config.toml"),
                prepared.root.join("grok-prompt"),
            );
            cli::private_file(&config, config_for(prepared.login))
                .and_then(|_| cli::private_file(&prompt, prepared.prompt.as_deref().unwrap_or_default()))
                .map_err(|_| Failure::Failed("Cannot prepare Grok configuration".into()))?;
            // The container's own mount failure reads as a credential problem; the
            // others are checked first so each keeps its own message.
            container(prepared.name, &node, prepared.login, prepared.root, &image)
                .map_err(|_| blocked("Grok credential metadata unavailable"))?;
            [&config, &prompt, prepared.mailbox]
                .into_iter()
                .try_for_each(|path| cli::checked(path).map(drop))
                .map_err(|_| blocked("Invalid Grok mount path"))?;
            turn_container(
                prepared.name,
                &node,
                prepared.login,
                prepared.root,
                &config,
                &prompt,
                prepared.mailbox,
                &image,
                &model,
            )
            .map_err(|_| blocked("Grok credential metadata unavailable"))
        })
    }
}
const MAILBOX: &str = "grok-mailbox";
/// Where a turn mounts a read-only login.
const AUTH: &str = "/home/agent/.grok/auth.json";
/// Where a refreshable sign-in's own folder is mounted, and what stays in it.
/// Grok replaces `auth.json` by writing `.<pid>.<n>.tmp` beside it and renaming
/// it over the file, serialises refreshes across processes with an
/// `auth.json.lock` it never deletes, re-reads the file under that lock, and
/// backs a corrupt file up as `auth.json.corrupt.<millis>`; `GROK_AUTH_PATH`
/// moves only that file (xai-grok-login `storage.rs` and `manager/lock.rs`,
/// xai-grok-config `fs_atomic.rs`, at the pinned revision). Sessions, logs and
/// settings stay in `GROK_HOME` on the in-memory home.
const SIGN_IN: Folder = Folder {
    target: "/home/agent/.grok-login",
    file: "auth.json",
    variable: "GROK_AUTH_PATH",
    names: Names::File,
    keep: &[
        Keep::Name("auth.json"),
        Keep::Name("auth.json.lock"),
        Keep::Between(".", ".tmp"),
        Keep::Between("auth.json.corrupt.", ""),
        Keep::Name(KEY_FILE),
    ],
};
const LOGIN: &str = "run control grok login; its private login stays outside the checkout";
const PROBE: Probe = Probe {
    directory: "grok-isolation-probe",
    command: "set -eu; test ! -e /home/agent/.grok/auth.json; test ! -e /home/agent/.control-key; test ! -S /var/run/docker.sock; test ! -e /proc/1/root/home/agent/.grok/auth.json; test -z \"${XAI_API_KEY-}\"; printf verified > isolation-check",
    timed_out: "Grok tool isolation probe timed out after 90 s: the container engine is busy or slow, which is not an isolation failure. No account credentials were mounted; retry shortly.",
    failed: "Grok workspace isolation check failed; no credentials were mounted",
};
const CLI: Cli = Cli {
    name: "Grok",
    probe: &PROBE,
    mailbox: MAILBOX,
    no_login: "Grok login unavailable. Run control grok login; no other login on this host is used.",
    delivery: Delivery::File,
    keeps: Keeps::Folder(&SIGN_IN),
    capture: |reader, update, progress, bad_tools| {
        cli::capture_with(
            reader,
            control_ai::grok::Grok::agent(),
            "Grok",
            update,
            progress,
            bad_tools,
        )
    },
    failure: diagnose,
};
/// Headless, unattended and stateless: no update check, subagents, web search,
/// memory or plan mode, and every remaining tool call approved.
const FLAGS: [&str; 9] = [
    "--output-format",
    "streaming-json",
    "--no-auto-update",
    "--always-approve",
    "--no-subagents",
    "--disable-web-search",
    "--no-memory",
    "--no-plan",
    "--verbatim",
];
/// An allowlist naming no built-in tool. `mcp__` entries are never unknown to
/// Grok, so this cannot trip its fall-back to the full toolset; what remains is
/// the two MCP meta-tools.
const NO_TOOLS: &str = "mcp__control";
/// Control's tools, as the one MCP server in Grok's user configuration.
const CONFIG: &str = "[mcp_servers.control]\ncommand = \"/bin/control-room-mcp\"\n";
/// The same server started without the key Grok was given: Grok passes its
/// own environment to an MCP server, and Control's server needs no key.
const KEY_CONFIG: &str =
    "[mcp_servers.control]\ncommand = \"/bin/env\"\nargs = [\"-u\", \"XAI_API_KEY\", \"/bin/control-room-mcp\"]\n";
/// The name of an API-key account's key file, in its directory and here.
const KEY_FILE: &str = "xai-api-key";
const KEY: &str = "/home/agent/.control-key/xai-api-key";
/// The MCP configuration for the login's form.
fn config_for(login: &Granted) -> &'static str {
    match login.form() {
        Form::ApiKey | Form::Proxied => KEY_CONFIG,
        Form::Session | Form::CloudDefault => CONFIG,
    }
}
/// What runs Grok: the CLI itself for a sign-in. For a key, `control-grok`
/// (tools/grok), which exports the mounted key as XAI_API_KEY, Grok's
/// documented key path, for Grok's process alone: the key is never a
/// variable of the container.
fn program(login: &Granted) -> &'static str {
    match login.form() {
        Form::ApiKey | Form::Proxied => "control-grok",
        Form::Session | Form::CloudDefault => "grok",
    }
}

fn fail(message: &str) -> io::Error {
    io::Error::other(message)
}
/// The shared CLI container with Grok's home on a tmpfs, so sessions and logs
/// die with it, and the login mounted as the host granted it. A proxied key
/// is its placeholder at the key's place, written in the attempt's `private`
/// directory, which `control-grok` exports as XAI_API_KEY; Grok is pointed at
/// the relay (`GROK_XAI_API_BASE_URL`, whose `/api-key` probe and model routes
/// the relay allows) and kept from its updater and telemetry.
fn container(name: &str, node: &str, login: &Granted, private: &Path, image: &str) -> io::Result<RunSpec> {
    let owner = login.owner();
    let spec = RunSpec {
        image: image.into(),
        input: Input::Attached,
        ..cli::base(name, node, owner)
    }
    .env("GROK_HOME", "/home/agent/.grok")
    .tmpfs(Tmpfs::new("/home/agent/.grok", Size::Mib(64)).owned(owner));
    match login.form() {
        Form::ApiKey => login.mount(spec, "grok/api-key", KEY),
        Form::Proxied => {
            let relay = login.relay()?;
            Ok(relay
                .place(spec, &private.join(cli::PLACEHOLDER), relay.placeholder(), KEY)?
                .env("GROK_XAI_API_BASE_URL", &format!("{}/v1", relay.origin()))
                .env("GROK_DISABLE_AUTOUPDATER", "1")
                .env("GROK_TELEMETRY_ENABLED", "0"))
        }
        Form::Session | Form::CloudDefault => {
            login.mount_session(spec, "grok/auth.json", AUTH, Keeps::Folder(&SIGN_IN))
        }
    }
}
/// A proxied turn has no network and starts once its relay listens.
fn offline(spec: RunSpec, login: &Granted) -> io::Result<RunSpec> {
    match login.form() {
        Form::Proxied => Ok(login.relay()?.offline(spec)),
        Form::ApiKey | Form::Session | Form::CloudDefault => Ok(spec),
    }
}
fn text_container(call: &TextCall<'_>, login: &Granted) -> io::Result<RunSpec> {
    if login.form() == Form::Proxied {
        return Err(fail("A text call never uses a proxied key"));
    }
    // A text request carries no prompt file: the caller writes the prompt to
    // standard input, which `sh` hands to Grok as a file it can read.
    Ok(container(call.name, call.node, login, call.workspace, call.image)?
        .bind(cli::checked(call.workspace)?, "/workspace", Access::ReadOnly)
        .args([
            "bash".to_owned(),
            "-c".to_owned(),
            format!(
                "cat > /tmp/prompt && exec {} \"$@\" --prompt-file /tmp/prompt",
                program(login)
            ),
            "grok".to_owned(),
        ])
        .args(FLAGS)
        .args([
            "--tools",
            NO_TOOLS,
            "--disallowed-tools",
            "search_tool,use_tool",
            "--model",
            call.model,
        ]))
}
/// One turn's container: Grok's configuration and prompt read-only, the mailbox writable.
#[allow(clippy::too_many_arguments)]
fn turn_container(
    name: &str,
    node: &str,
    login: &Granted,
    private: &Path,
    config: &Path,
    turn: &Path,
    mailbox: &Path,
    image: &str,
    model: &str,
) -> io::Result<RunSpec> {
    let spec = container(name, node, login, private, image)?
        .bind(cli::checked(config)?, "/home/agent/.grok/config.toml", Access::ReadOnly)
        .bind(cli::checked(turn)?, "/home/agent/prompt", Access::ReadOnly)
        .bind(cli::checked(mailbox)?, "/workspace", Access::ReadWrite)
        .args([program(login), "--prompt-file", "/home/agent/prompt"])
        .args(FLAGS)
        .args(["--tools", NO_TOOLS, "--model", model]);
    offline(spec, login)
}
/// The credential canary in a turn's own container, for each form of login,
/// on a fake credential.
fn canary(isolation: &dyn Isolation, node: &str, root: &Path, image: &str) -> io::Result<()> {
    let (config, prompt, mailbox) = (
        root.join("grok-config.toml"),
        root.join("grok-prompt"),
        root.join(MAILBOX),
    );
    cli::private_file(&prompt, "")?;
    fs::create_dir_all(&mailbox)?;
    let name = format!("control-grok-probe-{}-canary", std::process::id());
    [
        (Canary::ReadOnly(Form::Session), AUTH),
        (Canary::Refreshable(Keeps::Folder(&SIGN_IN)), AUTH),
        (Canary::ReadOnly(Form::ApiKey), KEY),
    ]
    .into_iter()
    .try_for_each(|(login, target)| {
        cli::canary_check(isolation, root, login, target, |login| {
            cli::private_file(&config, config_for(login))?;
            turn_container(&name, node, login, root, &config, &prompt, &mailbox, image, "canary")
        })
    })?;
    cli::proxied_canary_check(isolation, root, KEY, |login| {
        cli::private_file(&config, config_for(login))?;
        turn_container(&name, node, login, root, &config, &prompt, &mailbox, image, "canary")
    })
}
fn login_container(name: &str, node: &str, owner: User, directory: &Path, image: &str) -> io::Result<RunSpec> {
    Ok(RunSpec {
        image: image.into(),
        input: Input::Terminal,
        ..cli::base(name, node, owner)
    }
    .env("GROK_HOME", "/home/agent/.grok")
    .tmpfs(Tmpfs::new("/workspace", Size::Mib(1)))
    // The sign-in alone is kept, in the account's folder, where turns find it;
    // the rest of what the login writes stays on the in-memory home.
    .secret(
        "grok/login",
        cli::checked(directory)?,
        SIGN_IN.target,
        Access::ReadWrite,
    )
    .env(SIGN_IN.variable, &SIGN_IN.credential())
    .args(["grok", "login", "--device-auth"]))
}
/// A fixed message for a failure the shared classifier knows (a usage limit, a
/// rejected login); otherwise Grok's own bounded ending.
fn diagnose(ended: Option<Ending>, stderr: &[u8]) -> Failure {
    let known = |bytes: &[u8]| control_agent::classify(bytes).map(|cause| Failure::Provider { agent: "Grok", cause });
    // A rejected tool is never read as a provider cause, whatever its wording.
    ended
        .as_ref()
        .filter(|e| !e.rejected_tools)
        .and_then(|e| known(e.text.as_bytes()))
        .or_else(|| ended.map(Ending::failure))
        .or_else(|| known(stderr))
        .unwrap_or(Failure::Unclassified(
            "Grok stopped without success. Check its login, selected model and usage limits.",
        ))
}

/// `control grok login | check`.
pub fn command(command: Command<'_>) -> io::Result<()> {
    const USAGE: &str = "Use control grok login | check";
    let Command {
        args,
        node,
        isolation,
        image,
        directory,
        ..
    } = command;
    let [action] = args else { return Err(fail(USAGE)) };
    if matches!(action.as_str(), "--help" | "-h" | "help") {
        println!(
            "control grok login: sign in with a device code inside the pinned Nix Grok container\ncontrol grok key --account NAME: save an API-key account's key from standard input\ncontrol grok check: offline tool isolation check, then a fake-credential canary in a turn container for each login form; no real credentials or model calls"
        );
        return Ok(());
    }
    let image = || {
        image
            .clone()
            .ok_or_else(|| fail("Build and pin the Grok image first: nix run .#setup -- --grok"))
    };
    let node_id = || node.id().map_err(|e| fail(&e));
    match action.as_str() {
        "login" => {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new().recursive(true).mode(0o700).create(&directory)?;
            let meta = fs::symlink_metadata(&directory)?;
            if !meta.is_dir() || meta.mode() & 0o077 != 0 || meta.uid() == 0 {
                return Err(fail(
                    "Grok login directory must be private and owned by a non-root user",
                ));
            }
            let spec = login_container(
                &format!("control-grok-login-{}", std::process::id()),
                &node_id()?,
                User {
                    uid: meta.uid(),
                    gid: meta.gid(),
                },
                &directory,
                &image()?,
            )?;
            if !isolation.spawn(&spec, Streams::INHERIT)?.wait()?.success() {
                return Err(fail("Grok login did not complete"));
            }
            // Signing in again replaces the sign-in in place; whatever an earlier
            // login kept beside it (sessions, logs, settings) goes.
            control_agent::sweep(&directory, SIGN_IN.keep)?;
            fs::set_permissions(directory.join(SIGN_IN.file), fs::Permissions::from_mode(0o600))
        }
        "check" => {
            let (image, node_id) = (image()?, node_id()?);
            let root = node.check_root("grok-check").map_err(|e| fail(&e))?;
            cli::probe(
                node,
                isolation,
                &image,
                &node_id,
                &root,
                &format!("control-grok-probe-{}", std::process::id()),
                &PROBE,
            )?;
            println!("Grok offline workspace isolation verified. No account credentials were mounted or model called.");
            canary(isolation, &node_id, &root, &image)
        }
        _ => Err(fail(USAGE)),
    }
}
#[cfg(test)]
mod tests;
