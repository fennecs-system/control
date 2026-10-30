//! Scoped GitHub publication. The trusted broker consumes a private credential;
//! models select immutable outputs, never a remote, token, host path or Git command.
use crate::{
    identity::{Action, Actor},
    usage::Attempt,
    State,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

static PUBLICATION: Mutex<()> = Mutex::new(());
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    pub repository: String,
    pub source_root: PathBuf,
    pub token_file: PathBuf,
    pub base: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Source {
    pub repository: String,
    pub source_root: PathBuf,
    pub commit: String,
    pub baseline: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Publication {
    pub run: String,
    pub artifact: String,
    pub repository: String,
    pub branch: String,
    pub base: String,
    pub base_commit: String,
    pub tree: Option<String>,
    pub commit: Option<String>,
    pub url: Option<String>,
    pub status: String,
    pub title: String,
    pub body: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub run_id: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
}
fn fail(message: &str) -> io::Error {
    io::Error::other(message)
}
fn sha(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn repository(value: &str) -> bool {
    let parts: Vec<_> = value.split('/').collect();
    parts.len() == 2
        && parts.iter().all(|p| {
            !p.is_empty()
                && *p != "."
                && *p != ".."
                && p.len() <= 100
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        })
}
fn branch(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 200
        && !value.contains("..")
        && !value.ends_with(['/', '.', ' '])
        && !value.starts_with(['/', '.'])
        && value
            .split('/')
            .all(|p| !p.is_empty() && !p.starts_with('.') && !p.ends_with(".lock"))
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_/ .".contains(&b))
        && !value.contains(' ')
}
fn read_private(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    crate::config::read_regular(path, usize::try_from(limit).map_err(|_| fail("Invalid file limit"))?, true)
        .map_err(|_| fail("GitHub configuration and credentials must be bounded private regular files"))
}
fn configuration() -> io::Result<Option<Configuration>> {
    let path = crate::config::directory()
        .map_err(fail)?
        .join("github.json");
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
        Ok(_) => {}
    }
    let value: Configuration = serde_json::from_slice(&read_private(&path, 16384)?)
        .map_err(|_| fail("Invalid github.json configuration"))?;
    validate_configuration(&value)?;
    Ok(Some(value))
}
fn validate_configuration(value: &Configuration) -> io::Result<()> {
    if !repository(&value.repository)
        || !branch(&value.base)
        || !value.source_root.is_absolute()
        || !value.token_file.is_absolute()
    {
        return Err(fail(
            "GitHub binding requires owner/repository, a valid base branch and absolute paths",
        ));
    }
    let root = crate::config::source_root()?;
    let credential = value
        .token_file
        .canonicalize()
        .map_err(|_| fail("GitHub credential reference is unavailable"))?;
    if root != value.source_root
        || credential.starts_with(&root)
        || credential.starts_with(crate::config::state_directory().map_err(fail)?)
    {
        return Err(fail("GitHub source must match the authorized improvement root; credentials must be outside source and state"));
    }
    Ok(())
}
pub fn cli(args: &[String]) -> io::Result<()> {
    if args.first().map(String::as_str) != Some("configure") {
        println!("control github configure --repository OWNER/REPO --token-file /private/path [--base main]\nBinds GitHub publication to the configured improvement source. Credential contents are never displayed.");
        return Ok(());
    }
    let option = |name: &str| args.windows(2).find(|p| p[0] == name).map(|p| p[1].clone());
    let value = Configuration {
        repository: option("--repository").ok_or_else(|| fail("Supply --repository OWNER/REPO"))?,
        source_root: crate::config::source_root()?,
        token_file: PathBuf::from(
            option("--token-file").ok_or_else(|| {
                fail("Supply --token-file with an absolute private credential path")
            })?,
        ),
        base: option("--base").unwrap_or_else(|| "main".into()),
    };
    validate_configuration(&value)?;
    // Metadata only during setup. Only the API adapter consumes the credential.
    use std::os::unix::fs::PermissionsExt;
    let meta = fs::symlink_metadata(&value.token_file)
        .map_err(|_| fail("Credential file is unavailable"))?;
    if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 || meta.len() > 4096 {
        return Err(fail(
            "Credential must be a private regular file, at most 4 KiB",
        ));
    }
    crate::config::write_private("github.json", &serde_json::to_vec_pretty(&value)?)?;
    println!(
        "GitHub publication configured for {}. No merge or deployment permission was granted.",
        value.repository
    );
    Ok(())
}
pub fn inherit(state: &State, parent: Option<&str>) -> Option<Source> {
    let parent = parent?;
    state
        .runtime
        .runs
        .iter()
        .filter(|r| r.action == Action::Improve)
        .flat_map(|r| &r.attempts)
        .find(|a| a.artifact.as_deref() == Some(parent))
        .and_then(|a| a.github_source.clone())
}
pub fn prepare(attempt: &mut Attempt, baseline: &Path) -> io::Result<()> {
    if attempt.github_source.is_some() {
        return Ok(());
    }
    let Some(config) = configuration()? else {
        return Ok(());
    };
    let id = format!(
        "github-base-{}",
        attempt
            .workspace_id
            .as_deref()
            .ok_or_else(|| fail("Missing attempt identity"))?
    );
    let manifest = crate::artifacts::publish(baseline, &id)?;
    let paths: Vec<_> = manifest
        .files
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    let commit = verify_source(&config.source_root, &paths, |path| {
        crate::artifacts::read(&manifest, path)
    })?;
    attempt.github_source = Some(Source {
        repository: config.repository,
        source_root: config.source_root,
        commit,
        baseline: id,
    });
    Ok(())
}
// This Git process only inspects the operator-bound checkout, never agent files.
fn source_git(root: &Path, args: &[&str]) -> io::Result<Vec<u8>> {
    const LIMIT: u64 = 2 * 1024 * 1024;
    let mut command = std::process::Command::new("git");
    command
        .env_clear()
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-C",
        ])
        .arg(root)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    let mut child = command
        .spawn()
        .map_err(|_| fail("Cannot capture the source Git revision"))?;
    let reader = child
        .stdout
        .take()
        .ok_or_else(|| fail("Missing Git output"))?;
    let output = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        reader
            .take(LIMIT + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait()? {
            Some(s) => break s,
            None if started.elapsed() < Duration::from_secs(5) => {
                std::thread::sleep(Duration::from_millis(10))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = output.join();
                return Err(fail("Source revision lookup timed out"));
            }
        }
    };
    let bytes = output
        .join()
        .map_err(|_| fail("Git output reader stopped"))??;
    if !status.success() || bytes.len() as u64 > LIMIT {
        return Err(fail("Source Git inspection failed or exceeded its bound"));
    }
    Ok(bytes)
}
fn verify_source(
    root: &Path,
    paths: &[&str],
    mut read: impl FnMut(&str) -> io::Result<Vec<u8>>,
) -> io::Result<String> {
    let head = source_git(root, &["rev-parse", "--verify", "HEAD"])?;
    let commit = std::str::from_utf8(&head).unwrap_or("").trim();
    if !sha(commit) {
        return Err(fail("Source must have a committed Git baseline"));
    }
    let clean = || -> io::Result<()> {
        if !source_git(
            root,
            &["status", "--porcelain=v1", "--untracked-files=normal"],
        )?
        .is_empty()
        {
            return Err(fail("GitHub publication requires a clean committed source checkout; commit or move local changes, then start a fresh improvement"));
        }
        Ok(())
    };
    clean()?;
    let listing = source_git(root, &["ls-tree", "-rz", "--full-tree", commit])?;
    let mut objects = BTreeMap::new();
    for record in listing.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let record =
            std::str::from_utf8(record).map_err(|_| fail("Source Git paths must be UTF-8"))?;
        let (metadata, path) = record
            .split_once('\t')
            .ok_or_else(|| fail("Invalid Git tree"))?;
        let fields: Vec<_> = metadata.split_whitespace().collect();
        if fields.len() != 3 {
            return Err(fail("Invalid Git tree"));
        }
        objects.insert(path, (fields[0], fields[1], fields[2]));
    }
    // Status omits ignored files. Verify every included snapshot file against
    // the pinned Git object instead of silently trusting ignored dependencies.
    for path in paths {
        let Some((mode, kind, hash)) = objects.get(path) else {
            return Err(fail("Source baseline contains an uncommitted or ignored input; commit it or exclude it before starting an improvement"));
        };
        let bytes = read(path)?;
        if !matches!(*mode, "100644" | "100755") || *kind != "blob" || git_blob(&bytes) != *hash {
            return Err(fail("Source baseline differs from its committed Git objects; refresh the checkout and start a fresh improvement"));
        }
    }
    clean()?;
    if source_git(root, &["rev-parse", "--verify", "HEAD"])? != head {
        return Err(fail(
            "Source revision changed during capture; start a fresh improvement",
        ));
    }
    Ok(commit.into())
}
pub fn definition() -> Value {
    json!({"name":"control_github_open_pr","description":"Publish a completed successful improvement checkpoint to the operator-bound GitHub repository and open a draft pull request. Uses private broker credentials; never merges, force-pushes, edits the host checkout or accepts arbitrary destinations. Select a previous completed improvement run in this chat.","inputSchema":{"type":"object","properties":{"run_id":{"type":"string"},"title":{"type":"string","maxLength":200},"body":{"type":"string","maxLength":16000}},"required":["run_id","title"],"additionalProperties":false},"annotations":{"readOnlyHint":false,"destructiveHint":false,"openWorldHint":true}})
}
fn validate_input(input: &Input) -> io::Result<()> {
    if !crate::chat::valid_id(&input.run_id)
        || input.title.trim().is_empty()
        || input.title.len() > 200
        || input.title.contains(['\n', '\r'])
        || input.body.len() > 16000
    {
        return Err(fail(
            "Expected a completed run, a short title and optional bounded description",
        ));
    }
    Ok(())
}
fn selected(state: &State, chat: &str, input: &Input) -> io::Result<(Attempt, Actor)> {
    validate_input(input)?;
    let session = state
        .chats
        .iter()
        .find(|c| c.id == chat)
        .ok_or_else(|| fail("Chat not found"))?;
    crate::identity::authorize(&session.actor, Action::Improve).map_err(fail)?;
    let run = state
        .runtime
        .runs
        .iter()
        .find(|r| {
            r.id == input.run_id
                && r.chat_id.as_deref() == Some(chat)
                && r.actor == session.actor
                && r.action == Action::Improve
                && r.status == "completed"
        })
        .ok_or_else(|| fail("Select a completed improvement in this chat"))?;
    if crate::delegation::is_judge(run) {
        return Err(fail("Judge runs cannot publish improvements"));
    }
    let attempt=run.attempts.last().filter(|a|a.status=="completed"&&!a.review_only&&a.github_source.is_some()&&a.artifact.is_some()).ok_or_else(||fail("This run has no captured GitHub source/checkpoint; configure GitHub and start a new improvement"))?;
    Ok((attempt.clone(), run.actor.clone()))
}
#[derive(Clone)]
struct Change {
    path: String,
    old: Option<Vec<u8>>,
    new: Option<Vec<u8>>,
}
fn changes(source: &Source, artifact: &str) -> io::Result<Vec<Change>> {
    let before = crate::artifacts::manifest(&source.baseline)?;
    let after = crate::artifacts::manifest(artifact)?;
    if !before.omitted.is_empty() || !after.omitted.is_empty() {
        return Err(fail("Cannot publish an incomplete checkpoint"));
    }
    let mut paths = BTreeMap::new();
    for entry in &before.files {
        paths.insert(entry.path.clone(), (Some(entry.sha256.clone()), None));
    }
    for entry in &after.files {
        paths.entry(entry.path.clone()).or_insert((None, None)).1 = Some(entry.sha256.clone());
    }
    let mut result = Vec::new();
    let mut bytes = 0;
    for (path, (old, new)) in paths {
        if old == new {
            continue;
        }
        if result.len() >= 64 {
            return Err(fail("GitHub publication is limited to 64 changed files"));
        }
        let old = old
            .map(|_| crate::artifacts::read(&before, &path))
            .transpose()?;
        let new = new
            .map(|_| crate::artifacts::read(&after, &path))
            .transpose()?;
        bytes += new.as_ref().map_or(0, Vec::len);
        if bytes > 512 * 1024 {
            return Err(fail(
                "GitHub publication exceeds the 512 KiB changed-content limit",
            ));
        }
        if new
            .as_ref()
            .is_some_and(|b| std::str::from_utf8(b).is_err())
        {
            return Err(fail("This publisher supports text changes only"));
        }
        result.push(Change { path, old, new });
    }
    if result.is_empty() {
        return Err(fail("No changes to publish"));
    }
    Ok(result)
}
fn git_blob(bytes: &[u8]) -> String {
    let mut context = ring::digest::Context::new(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY);
    context.update(format!("blob {}\0", bytes.len()).as_bytes());
    context.update(bytes);
    context
        .finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
trait Api {
    fn request(&self, method: &str, path: &str, body: Option<Value>) -> io::Result<Value>;
}
struct Rest {
    repository: String,
    client: reqwest::blocking::Client,
    authorization: reqwest::header::HeaderValue,
}
impl Rest {
    fn new(config: &Configuration) -> io::Result<Self> {
        let bytes = read_private(&config.token_file, 4096)?;
        let token = std::str::from_utf8(&bytes)
            .map_err(|_| fail("Invalid GitHub credential"))?
            .trim();
        if token.is_empty() || token.contains(char::is_whitespace) {
            return Err(fail("Invalid GitHub credential"));
        }
        let mut authorization = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| fail("Invalid GitHub credential"))?;
        authorization.set_sensitive(true);
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|_| fail("Cannot initialize GitHub client"))?;
        Ok(Self {
            repository: config.repository.clone(),
            client,
            authorization,
        })
    }
}
impl Api for Rest {
    fn request(&self, method: &str, path: &str, body: Option<Value>) -> io::Result<Value> {
        if !path.starts_with(&format!("/repos/{}/", self.repository))
            || !["GET", "POST"].contains(&method)
        {
            return Err(fail(
                "GitHub request is outside the configured repository capability",
            ));
        }
        let method = reqwest::Method::from_bytes(method.as_bytes())
            .map_err(|_| fail("Invalid API method"))?;
        let mut request = self
            .client
            .request(method, format!("https://api.github.com{path}"))
            .header("Authorization", self.authorization.clone())
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2026-03-10")
            .header("User-Agent", "control");
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().map_err(|_| {
            fail("GitHub request failed; inspect retained publication before retrying")
        })?;
        let status = response.status();
        if status.as_u16() == 404 {
            return Ok(Value::Null);
        }
        if !status.is_success() {
            return Err(fail("GitHub rejected the request; check repository permissions and retained publication status"));
        }
        let mut bytes = Vec::new();
        response
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| fail("Could not read GitHub response"))?;
        if bytes.len() > 1024 * 1024 {
            return Err(fail("GitHub response exceeds its bound"));
        }
        serde_json::from_slice(&bytes).map_err(|_| fail("Invalid GitHub response"))
    }
}
struct Scoped<'a> {
    rest: Rest,
    actor: &'a Actor,
    configuration: &'a Configuration,
}
impl Api for Scoped<'_> {
    fn request(&self, method: &str, path: &str, body: Option<Value>) -> io::Result<Value> {
        if crate::shutdown::requested() {
            return Err(fail("Control is stopping; publication intent is retained"));
        }
        crate::identity::authorize(self.actor, Action::Improve).map_err(fail)?;
        if configuration()?.as_ref() != Some(self.configuration) {
            return Err(fail("GitHub binding changed; publication was stopped"));
        }
        self.rest.request(method, path, body)
    }
}
fn encoded(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
fn request_path(repo: &str, suffix: &str) -> String {
    format!("/repos/{repo}/{suffix}")
}
fn object_sha(value: &Value, key: &str) -> io::Result<String> {
    value[key]
        .as_str()
        .filter(|s| sha(s))
        .map(str::to_owned)
        .ok_or_else(|| fail("GitHub returned an invalid object identity"))
}
fn create_tree(
    api: &impl Api,
    publication: &Publication,
    changes: &[Change],
) -> io::Result<String> {
    let repo = &publication.repository;
    let commit = api.request(
        "GET",
        &request_path(repo, &format!("git/commits/{}", publication.base_commit)),
        None,
    )?;
    let tree = object_sha(&commit["tree"], "sha")?;
    // Inspect each changed path, never infer deletion from a truncated recursive listing.
    let mut entries = Vec::new();
    let mut trees = std::collections::HashMap::new();
    for change in changes {
        let path = change
            .path
            .split('/')
            .map(encoded)
            .collect::<Vec<_>>()
            .join("/");
        let old = api.request(
            "GET",
            &request_path(
                repo,
                &format!("contents/{path}?ref={}", publication.base_commit),
            ),
            None,
        )?;
        let old_sha = old["sha"].as_str();
        if old_sha != change.old.as_ref().map(|b| git_blob(b)).as_deref() {
            return Err(fail(
                "Source baseline differs from GitHub; refresh source and start a new improvement",
            ));
        }
        if !old.is_null() && old["type"] != "file" {
            return Err(fail("Changed path is not a regular GitHub file"));
        }
        let mode = if old.is_null() {
            "100644"
        } else {
            // File contents APIs omit executable mode; query the containing tree.
            let mut current = tree.clone();
            let components: Vec<_> = change.path.split('/').collect();
            let mut found = None;
            for (index, name) in components.iter().enumerate() {
                if !trees.contains_key(&current) {
                    // Bound retained API responses even for changes spanning many directories.
                    if trees.len() == 32 { trees.clear(); }
                    let listing = api.request("GET", &request_path(repo, &format!("git/trees/{current}")), None)?;
                    trees.insert(current.clone(), listing);
                }
                let listing = &trees[&current];
                if listing["truncated"] == true {
                    return Err(fail("GitHub tree listing is truncated"));
                }
                let entry = listing["tree"]
                    .as_array()
                    .and_then(|a| a.iter().find(|e| e["path"].as_str() == Some(name)))
                    .ok_or_else(|| fail("GitHub path is missing from its tree"))?;
                if index + 1 == components.len() {
                    found = entry["mode"].as_str().map(str::to_owned);
                } else {
                    current = object_sha(entry, "sha")?;
                }
            }
            match found.as_deref() {
                Some("100755") => "100755",
                Some("100644") => "100644",
                _ => return Err(fail("Changed GitHub path has an unsupported mode")),
            }
        };
        let mut entry = json!({"path":change.path,"mode":mode,"type":"blob"});
        match &change.new {
            Some(bytes) => {
                entry["content"] =
                    json!(std::str::from_utf8(bytes).map_err(|_| fail("Text publication only"))?)
            }
            None => entry["sha"] = Value::Null,
        }
        entries.push(entry);
    }
    let response = api.request(
        "POST",
        &request_path(repo, "git/trees"),
        Some(json!({"base_tree":tree,"tree":entries})),
    )?;
    object_sha(&response, "sha")
}
fn finish(
    api: &impl Api,
    publication: &mut Publication,
    changes: &[Change],
    mut checkpoint: impl FnMut(&Publication) -> io::Result<()>,
) -> io::Result<String> {
    if !repository(&publication.repository)
        || !branch(&publication.branch)
        || !branch(&publication.base)
        || !sha(&publication.base_commit)
        || publication.tree.as_ref().is_some_and(|s| !sha(s))
        || publication.commit.as_ref().is_some_and(|s| !sha(s))
    {
        return Err(fail("Invalid captured publication identity"));
    }
    let base = api.request(
        "GET",
        &request_path(
            &publication.repository,
            &format!("git/ref/heads/{}", encoded(&publication.base)),
        ),
        None,
    )?;
    if base["object"]["sha"].as_str() != Some(&publication.base_commit) {
        return Err(fail("Configured GitHub base differs from the captured source revision; refresh the clean source checkout from that base and start a fresh improvement"));
    }
    if publication.tree.is_none() {
        publication.tree = Some(create_tree(api, publication, changes)?);
        checkpoint(publication)?;
    }
    if publication.commit.is_none() {
        let result=api.request("POST",&request_path(&publication.repository,"git/commits"),Some(json!({"message":publication.title,"tree":publication.tree,"parents":[publication.base_commit]})))?;
        publication.commit = Some(object_sha(&result, "sha")?);
        checkpoint(publication)?;
    }
    let reference = request_path(
        &publication.repository,
        &format!("git/ref/heads/{}", encoded(&publication.branch)),
    );
    let existing = api.request("GET", &reference, None)?;
    if existing.is_null() {
        api.request(
            "POST",
            &request_path(&publication.repository, "git/refs"),
            Some(
                json!({"ref":format!("refs/heads/{}",publication.branch),"sha":publication.commit}),
            ),
        )?;
    } else if existing["object"]["sha"].as_str() != publication.commit.as_deref() {
        return Err(fail("Publication branch changed; it was not overwritten"));
    }
    publication.status = "branch_created".into();
    checkpoint(publication)?;
    let owner = publication.repository.split('/').next().unwrap();
    let existing = api.request(
        "GET",
        &request_path(
            &publication.repository,
            &format!(
                "pulls?state=all&head={}&base={}",
                encoded(&format!("{owner}:{}", publication.branch)),
                encoded(&publication.base)
            ),
        ),
        None,
    )?;
    let result = if let Some(first) = existing.as_array().and_then(|a| a.first()) {
        first.clone()
    } else {
        api.request("POST",&request_path(&publication.repository,"pulls"),Some(json!({"title":publication.title,"body":publication.body,"head":publication.branch,"base":publication.base,"draft":true,"maintainer_can_modify":false})))?
    };
    let url = result["html_url"]
        .as_str()
        .ok_or_else(|| fail("GitHub did not return a pull request URL"))?;
    let parsed = reqwest::Url::parse(url).map_err(|_| fail("Invalid GitHub pull request URL"))?;
    if parsed.scheme() != "https"
        || parsed.host_str() != Some("github.com")
        || !parsed
            .path()
            .starts_with(&format!("/{}/pull/", publication.repository))
    {
        return Err(fail("Invalid GitHub pull request destination"));
    }
    publication.url = Some(url.into());
    publication.status = "opened".into();
    checkpoint(publication)?;
    Ok(url.into())
}
pub fn open(shared: &Arc<Mutex<State>>, chat: &str, input: Input) -> io::Result<Value> {
    let _guard = PUBLICATION
        .try_lock()
        .map_err(|_| fail("A GitHub publication is already running"))?;
    let (attempt, actor) = {
        let state = shared.lock().map_err(|_| fail("State unavailable"))?;
        selected(&state, chat, &input)?
    };
    let config = configuration()?.ok_or_else(|| {
        fail("Configure a repository and credential reference with control github configure")
    })?;
    let source = attempt.github_source.as_ref().unwrap();
    if source.repository != config.repository || source.source_root != config.source_root {
        return Err(fail("Repository binding changed; start a new improvement"));
    }
    let artifact = attempt.artifact.as_deref().unwrap();
    let edits = changes(source, artifact)?;
    let mut publication = {
        let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
        selected(&state, chat, &input)?;
        if let Some(existing) = state.github_publications.iter().find(|p| {
            p.run == input.run_id && p.artifact == artifact && p.repository == config.repository
        }) {
            if existing.base != config.base || existing.base_commit != source.commit {
                return Err(fail(
                    "GitHub base binding changed; inspect retained publication",
                ));
            }
            if let Some(url) = &existing.url {
                return Ok(json!({"url":url,"status":existing.status}));
            }
            existing.clone()
        } else {
            let publication = Publication {
                run: input.run_id.clone(),
                artifact: artifact.into(),
                repository: config.repository.clone(),
                branch: format!("control/{}", input.run_id),
                base: config.base.clone(),
                base_commit: source.commit.clone(),
                tree: None,
                commit: None,
                url: None,
                status: "preparing".into(),
                title: input.title.trim().into(),
                body: input.body,
            };
            state.github_publications.push(publication.clone());
            if crate::storage::save(&state).is_err() {
                state.github_publications.pop();
                return Err(fail("Could not save publication intent"));
            }
            publication
        }
    };
    let api = Scoped {
        rest: Rest::new(&config)?,
        actor: &actor,
        configuration: &config,
    };
    let result = finish(&api, &mut publication, &edits, |publication| {
        if crate::shutdown::requested() {
            return Err(fail("Control is stopping; publication intent is retained"));
        }
        crate::identity::authorize(&actor, Action::Improve).map_err(fail)?;
        let mut state = shared.lock().map_err(|_| fail("State unavailable"))?;
        let index = state
            .github_publications
            .iter()
            .position(|p| {
                p.run == publication.run
                    && p.repository == publication.repository
                    && p.artifact == publication.artifact
            })
            .ok_or_else(|| fail("Publication intent missing"))?;
        let old = state.github_publications[index].clone();
        state.github_publications[index] = publication.clone();
        for c in state.chats.iter_mut().filter(|c| c.id == chat) {
            c.revision = c.revision.saturating_add(1);
        }
        if crate::storage::save(&state).is_err() {
            state.github_publications[index] = old;
            return Err(fail(
                "Could not checkpoint publication; inspect its retained intent",
            ));
        }
        Ok(())
    });
    match result {
        Ok(url) => Ok(json!({"url":url,"status":"opened"})),
        Err(error) => Err(error),
    }
}
static CONFIGURED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// UI availability only. Publication still revalidates configuration and authority.
/// Called by supervision outside the global state lock, including after config changes.
pub fn refresh_availability() {
    CONFIGURED.store(configuration().ok().flatten().is_some(), std::sync::atomic::Ordering::Relaxed);
}
pub fn panel(state: &State, chat: &str) -> String {
    let mut html = String::new();
    for publication in state.github_publications.iter().filter(|p| {
        state
            .runtime
            .runs
            .iter()
            .any(|r| r.id == p.run && r.chat_id.as_deref() == Some(chat))
    }) {
        if let Some(url) = &publication.url {
            html.push_str(&format!(
                "<p><a href='{}' target='_blank' rel='noopener noreferrer'>Pull request ↗</a></p>",
                crate::escape(url)
            ));
        }
    }
    if !CONFIGURED.load(std::sync::atomic::Ordering::Relaxed) {
        return html;
    }
    if let Some(run) = state.runtime.runs.iter().rev().find(|r| {
        r.chat_id.as_deref() == Some(chat)
            && r.action == Action::Improve
            && r.status == "completed"
            && r.attempts
                .last()
                .is_some_and(|a| a.github_source.is_some() && a.artifact.is_some())
    }) {
        if !state
            .github_publications
            .iter()
            .any(|p| p.run == run.id && p.url.is_some())
        {
            html.push_str(&format!("<details class='project-picker' data-transcript-key='github-publication'><summary>Open pull request</summary><form method='post' target='_top' action='/chat/{chat}/github'><input type='hidden' name='run_id' value='{}'><label>Title<input name='title' value='{}' maxlength='200' required></label><label>Description<textarea name='body' maxlength='16000'></textarea></label><button>Open draft pull request</button></form></details>",crate::escape(&run.id),crate::escape(&run.objective.chars().take(160).collect::<String>())));
        }
    }
    html
}
pub fn http(
    stream: &mut impl Write,
    shared: &Arc<Mutex<State>>,
    method: &str,
    path: &str,
    body: &[u8],
) -> bool {
    if path == "/mcp" && method == "POST" {
        let Ok(request) = serde_json::from_slice::<Value>(body) else {
            return false;
        };
        if request["method"] != "tools/call"
            || request["params"]["name"] != "control_github_open_pr"
        {
            return false;
        }
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let result = if id.is_null() || request["jsonrpc"] != "2.0" {
            Err(fail(
                "Publication requires an acknowledged JSON-RPC request",
            ))
        } else {
            serde_json::from_value::<Input>(request["params"]["arguments"].clone())
                .map_err(|_| fail("Expected run_id, title and optional body"))
                .and_then(|input| {
                    let chat = shared
                        .lock()
                        .map_err(|_| fail("State unavailable"))?
                        .runtime
                        .runs
                        .iter()
                        .find(|r| r.id == input.run_id)
                        .and_then(|r| r.chat_id.clone())
                        .ok_or_else(|| fail("Run not found"))?;
                    open(shared, &chat, input)
                })
        };
        let value = match result {
            Ok(value) => {
                json!({"jsonrpc":"2.0","id":id,"result":{"content":[{"type":"text","text":value.to_string()}],"structuredContent":{"data":value},"isError":false}})
            }
            Err(error) => {
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":error.to_string()}})
            }
        };
        let body = value.to_string();
        let _=write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",body.len());
        return true;
    }
    let Some(chat) = path
        .strip_prefix("/chat/")
        .and_then(|s| s.strip_suffix("/github"))
        .filter(|s| crate::chat::valid_id(s))
    else {
        return false;
    };
    let result = if method != "POST" {
        Err(fail("Use POST"))
    } else {
        let fields = crate::fields(&String::from_utf8_lossy(body));
        open(
            shared,
            chat,
            Input {
                run_id: fields.get("run_id").cloned().unwrap_or_default(),
                title: fields.get("title").cloned().unwrap_or_default(),
                body: fields.get("body").cloned().unwrap_or_default(),
            },
        )
    };
    match result {
        Ok(_) => {
            let _=write!(stream,"HTTP/1.1 303 See Other\r\nLocation: /chat/{chat}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
        Err(error) => {
            let body = crate::error_page(&error.to_string(), &format!("/chat/{chat}"));
            let _=write!(stream,"HTTP/1.1 400 Bad Request\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nContent-Security-Policy: default-src 'none'; style-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'\r\nConnection: close\r\n\r\n{body}",body.len());
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    const BASE: &str = "1111111111111111111111111111111111111111";
    const TREE: &str = "2222222222222222222222222222222222222222";
    const NEW_TREE: &str = "3333333333333333333333333333333333333333";
    const COMMIT: &str = "4444444444444444444444444444444444444444";
    struct Mock {
        requests: RefCell<Vec<(String, Option<Value>)>>,
        reference: RefCell<Option<String>>,
        pull: RefCell<Option<Value>>,
        fail: RefCell<Option<&'static str>>,
        old: Vec<u8>,
        base: String,
        tree_entries: Vec<Value>,
    }
    impl Mock {
        fn new() -> Self {
            Self {
                requests: RefCell::new(Vec::new()),
                reference: RefCell::new(None),
                pull: RefCell::new(None),
                fail: RefCell::new(None),
                old: b"old\n".to_vec(),
                base: BASE.into(),
                tree_entries: vec![json!({"path":"script.sh","sha":git_blob(b"old\n"),"mode":"100755"})],
            }
        }
    }
    impl Api for Mock {
        fn request(&self, method: &str, path: &str, body: Option<Value>) -> io::Result<Value> {
            assert!(path.starts_with("/repos/example/project/"));
            assert!(
                !["PATCH", "DELETE", "PUT"].contains(&method),
                "No overwrite, merge or deletion endpoints"
            );
            self.requests
                .borrow_mut()
                .push((format!("{method} {path}"), body.clone()));
            let result = if method == "GET" && path.contains("git/commits/") {
                json!({"tree":{"sha":TREE}})
            } else if method == "GET" && path.contains("contents/") {
                json!({"type":"file","sha":git_blob(&self.old)})
            } else if method == "GET" && path.contains("git/trees/") {
                json!({"tree":self.tree_entries,"truncated":false})
            } else if method == "POST" && path.ends_with("git/trees") {
                json!({"sha":NEW_TREE})
            } else if method == "POST" && path.ends_with("git/commits") {
                json!({"sha":COMMIT})
            } else if method == "GET" && path.ends_with("git/ref/heads/main") {
                json!({"object":{"sha":self.base}})
            } else if method == "GET" && path.contains("git/ref/") {
                self.reference
                    .borrow()
                    .as_ref()
                    .map_or(Value::Null, |sha| json!({"object":{"sha":sha}}))
            } else if method == "POST" && path.ends_with("git/refs") {
                *self.reference.borrow_mut() =
                    body.as_ref().unwrap()["sha"].as_str().map(str::to_owned);
                json!({"object":{"sha":COMMIT}})
            } else if method == "GET" && path.contains("pulls?") {
                self.pull
                    .borrow()
                    .as_ref()
                    .map_or(json!([]), |p| json!([p]))
            } else if method == "POST" && path.ends_with("pulls") {
                let value = json!({"html_url":"https://github.com/example/project/pull/7"});
                *self.pull.borrow_mut() = Some(value.clone());
                value
            } else {
                panic!("Unexpected mock route: {method} {path}")
            };
            if self
                .fail
                .borrow()
                .is_some_and(|suffix| method == "POST" && path.ends_with(suffix))
            {
                self.fail.borrow_mut().take();
                return Err(fail("Synthetic lost acknowledgement"));
            }
            Ok(result)
        }
    }
    fn publication() -> Publication {
        Publication {
            run: "r-fixture".into(),
            artifact: "a-fixture".into(),
            repository: "example/project".into(),
            branch: "control/r-fixture".into(),
            base: "main".into(),
            base_commit: BASE.into(),
            tree: None,
            commit: None,
            url: None,
            status: "preparing".into(),
            title: "Reviewable change".into(),
            body: "Synthetic verification".into(),
        }
    }
    fn edit() -> Vec<Change> {
        vec![Change {
            path: "script.sh".into(),
            old: Some(b"old\n".to_vec()),
            new: Some(b"new\n".to_vec()),
        }]
    }
    #[test]
    fn tree_cache_reuses_listings_and_preserves_each_files_mode() {
        let mut api = Mock::new();
        api.tree_entries.push(json!({"path":"module.rs","sha":git_blob(&api.old),"mode":"100644"}));
        let mut changes = edit();
        changes.push(Change {path:"module.rs".into(), old:Some(api.old.clone()), new:Some(b"updated".to_vec())});
        create_tree(&api, &publication(), &changes).unwrap();
        let requests = api.requests.borrow();
        assert_eq!(requests.iter().filter(|(path,_)| path.contains("GET /repos/example/project/git/trees/")).count(), 1);
        let body = requests.iter().find(|(path,_)|path == "POST /repos/example/project/git/trees").unwrap().1.as_ref().unwrap();
        assert_eq!(body["tree"][0]["mode"], "100755");
        assert_eq!(body["tree"][1]["mode"], "100644");
    }
    #[test]
    fn github_objects_and_refs_are_scoped_and_unambiguous() {
        for value in [
            "x/y/z",
            "../repo",
            "x/..",
            "https://github.com/x/y",
            "x/y?token=bad",
        ] {
            assert!(!repository(value));
        }
        for value in [
            "main..other",
            "/main",
            "a/.hidden",
            "main.lock",
            "refs@{1}",
            "main?x=y",
        ] {
            assert!(!branch(value));
        }
        assert!(repository("some-owner/a_repo"));
        assert!(branch("control/r-fixture"));
        assert_eq!(
            git_blob(b"test content\n"),
            "d670460b4b4aece5915caf5c68d12f560a9fe3e4"
        );
        assert_eq!(encoded("control/r?x"), "control%2Fr%3Fx");
        assert!(serde_json::from_value::<Input>(
            json!({"run_id":"r-x","title":"x","repository":"other/repo"})
        )
        .is_err());
    }
    #[test]
    fn publication_preserves_mode_and_creates_only_a_draft_without_overwriting() {
        let api = Mock::new();
        let mut publication = publication();
        let mut saved = Vec::new();
        let url = finish(&api, &mut publication, &edit(), |p| {
            saved.push(p.clone());
            Ok(())
        })
        .unwrap();
        assert_eq!(url, "https://github.com/example/project/pull/7");
        let requests = api.requests.borrow();
        let tree = requests
            .iter()
            .find(|(p, _)| p.ends_with("git/trees") && p.starts_with("POST"))
            .unwrap()
            .1
            .as_ref()
            .unwrap();
        assert_eq!(tree["base_tree"], TREE);
        assert_eq!(tree["tree"][0]["mode"], "100755");
        assert_eq!(tree["tree"][0]["content"], "new\n");
        let pull = requests
            .iter()
            .find(|(p, _)| p.ends_with("pulls") && p.starts_with("POST"))
            .unwrap()
            .1
            .as_ref()
            .unwrap();
        assert_eq!(pull["draft"], true);
        assert_eq!(pull["base"], "main");
        assert!(saved
            .iter()
            .any(|p| p.commit.as_deref() == Some(COMMIT) && p.status == "preparing"));
    }
    #[test]
    fn lost_branch_or_pr_acknowledgement_is_reconciled_without_duplicate_pr() {
        for suffix in ["git/refs", "pulls"] {
            let api = Mock::new();
            *api.fail.borrow_mut() = Some(suffix);
            let mut publication = publication();
            let mut durable = publication.clone();
            assert!(finish(&api, &mut publication, &edit(), |p| {
                durable = p.clone();
                Ok(())
            })
            .is_err());
            finish(&api, &mut durable, &edit(), |_| Ok(())).unwrap();
            assert_eq!(
                api.requests
                    .borrow()
                    .iter()
                    .filter(|(p, _)| p == "POST /repos/example/project/pulls")
                    .count(),
                1
            );
        }
    }
    #[test]
    fn changed_source_or_replaced_branch_is_never_overwritten() {
        let mut api = Mock::new();
        api.old = b"different\n".to_vec();
        let mut publication = publication();
        assert!(finish(&api, &mut publication, &edit(), |_| Ok(()))
            .unwrap_err()
            .to_string()
            .contains("baseline differs"));
        assert!(api
            .requests
            .borrow()
            .iter()
            .all(|(p, _)| p.starts_with("GET")));
        let api = Mock::new();
        *api.reference.borrow_mut() = Some("5555555555555555555555555555555555555555".into());
        let mut publication = super::tests::publication();
        publication.tree = Some(NEW_TREE.into());
        publication.commit = Some(COMMIT.into());
        assert!(finish(&api, &mut publication, &edit(), |_| Ok(()))
            .unwrap_err()
            .to_string()
            .contains("not overwritten"));
        assert!(api.pull.borrow().is_none());
    }
    #[test]
    fn diverged_remote_base_prevents_every_external_write() {
        let mut api = Mock::new();
        api.base = "5555555555555555555555555555555555555555".into();
        let mut publication = publication();
        assert!(finish(&api, &mut publication, &edit(), |_| Ok(()))
            .unwrap_err()
            .to_string()
            .contains("base differs"));
        assert!(api
            .requests
            .borrow()
            .iter()
            .all(|(method, _)| method.starts_with("GET")));
        assert!(publication.tree.is_none());
        assert!(api.reference.borrow().is_none());
        assert!(api.pull.borrow().is_none());
    }
    #[test]
    fn cumulative_turn_keeps_original_source_without_recapturing_host() {
        let source = Source {
            repository: "example/project".into(),
            source_root: "/operator-source".into(),
            commit: BASE.into(),
            baseline: "original-verified-baseline".into(),
        };
        let mut attempt = Attempt {
            github_source: Some(source.clone()),
            ..Default::default()
        };
        prepare(
            &mut attempt,
            Path::new("/deliberately-unavailable-later-baseline"),
        )
        .unwrap();
        assert_eq!(attempt.github_source, Some(source));
    }
    #[test]
    fn source_capture_rejects_dirty_untracked_ignored_and_stale_snapshot_inputs() {
        let root = std::env::temp_dir().join(format!(
            "control-github-source-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        source_git(&root, &["init", "--quiet"]).unwrap();
        fs::write(root.join("file.txt"), b"committed\n").unwrap();
        fs::write(root.join(".gitignore"), b"ignored.txt\n").unwrap();
        source_git(&root, &["add", "--", "file.txt", ".gitignore"]).unwrap();
        source_git(
            &root,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "Fixture",
            ],
        )
        .unwrap();
        let read = |path: &str| fs::read(root.join(path));
        assert!(verify_source(&root, &["file.txt"], read).is_ok());
        fs::write(root.join("file.txt"), b"dirty\n").unwrap();
        assert!(verify_source(&root, &["file.txt"], read)
            .unwrap_err()
            .to_string()
            .contains("clean committed"));
        fs::write(root.join("file.txt"), b"committed\n").unwrap();
        fs::write(root.join("untracked.txt"), b"untracked\n").unwrap();
        assert!(verify_source(&root, &["file.txt"], read)
            .unwrap_err()
            .to_string()
            .contains("clean committed"));
        fs::remove_file(root.join("untracked.txt")).unwrap();
        fs::write(root.join("ignored.txt"), b"ignored dependency\n").unwrap();
        assert!(source_git(
            &root,
            &["status", "--porcelain=v1", "--untracked-files=normal"]
        )
        .unwrap()
        .is_empty());
        assert!(verify_source(&root, &["file.txt", "ignored.txt"], read)
            .unwrap_err()
            .to_string()
            .contains("ignored input"));
        assert!(
            verify_source(&root, &["file.txt"], |_| Ok(b"stale snapshot\n".to_vec()))
                .unwrap_err()
                .to_string()
                .contains("committed Git objects")
        );
    }
    #[test]
    fn disk_failure_prevents_a_reachable_branch_or_pull_request() {
        let api = Mock::new();
        let mut publication = publication();
        assert!(finish(&api, &mut publication, &edit(), |_| Err(fail(
            "Synthetic disk full"
        )))
        .is_err());
        assert!(api.reference.borrow().is_none());
        assert!(api.pull.borrow().is_none());
    }
    #[test]
    fn only_completed_same_chat_improvements_can_be_published() {
        let mut state = State::default();
        let chat = crate::chat::create(&mut state);
        crate::chat::send(&mut state, &chat, "/improve Implement a fixture", "test").unwrap();
        let run = &mut state.runtime.runs[0];
        run.status = "completed".into();
        run.attempts.push(Attempt {
            status: "completed".into(),
            artifact: Some("a-fixture".into()),
            github_source: Some(Source {
                repository: "example/project".into(),
                source_root: "/fixture".into(),
                commit: BASE.into(),
                baseline: "original".into(),
            }),
            ..Default::default()
        });
        let input = Input {
            run_id: run.id.clone(),
            title: "Fixture".into(),
            body: String::new(),
        };
        assert!(selected(&state, &chat, &input).is_ok());
        state.runtime.runs[0].attempts[0].review_only = true;
        assert!(selected(&state, &chat, &input).is_err());
        state.runtime.runs[0].attempts[0].review_only = false;
        state.runtime.runs[0].persona = Some(crate::delegation::Persona {
            role: crate::delegation::Role::Judge,
            root: "fixture".into(),
            depth: 1,
            parent: Some("parent".into()),
        });
        assert!(selected(&state, &chat, &input).is_err());
        state.runtime.runs[0].persona = None;
        let other = crate::chat::create(&mut state);
        assert!(selected(&state, &other, &input).is_err());
        state.runtime.runs[0].action = Action::Task;
        assert!(selected(&state, &chat, &input).is_err());
        state.runtime.runs[0].action = Action::Improve;
        state.runtime.runs[0].status = "running".into();
        assert!(selected(&state, &chat, &input).is_err());
        state.chats[0].actor = Actor::Unknown;
        assert!(selected(&state, &chat, &input).is_err());
    }
}
