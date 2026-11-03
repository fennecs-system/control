use crate::identity::{Action, Actor, DiscordId, DmBinding, DmBindings, Policy};
use crate::{config, discord, identity};
use super::{permissions as discord_permissions, delivery::Snapshot};
use reqwest::{
    blocking::Client,
    header::{HeaderMap, HeaderValue, AUTHORIZATION},
};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const API: &str = "https://discord.com/api/v10";
#[derive(Clone)]
struct Route {
    channel: DiscordId,
    recipient: Option<DiscordId>,
}
impl Route {
    fn actor(&self, user: DiscordId) -> Option<Actor> {
        match self.recipient {
            Some(recipient) if recipient == user => Some(Actor::DiscordDm {
                user_id: user,
                channel_id: self.channel,
            }),
            Some(_) => None,
            None => None,
        }
    }
}
struct Bridge {
    client: Client,
    token: String,
    bot_id: DiscordId,
    bot_name: String,
    state: std::cell::RefCell<StateCache>,
}
/// Cache only delivery fields; reload on size, modification time or inode changes.
#[derive(Default)]
struct StateCache {
    stamp: Option<(SystemTime, u64, u64)>,
    value: std::rc::Rc<Snapshot>,
}
impl StateCache {
    fn load(&mut self, path: &Path) -> std::rc::Rc<Snapshot> {
        use std::os::unix::fs::MetadataExt;
        let stamp = fs::symlink_metadata(path).ok().filter(|m| m.is_file()).map(|m| (m.modified().unwrap_or(UNIX_EPOCH), m.len(), m.ino()));
        if stamp.is_none() || stamp != self.stamp {
            self.value = std::rc::Rc::new(
                config::read_regular(path, isize::MAX as usize, false).ok()
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                    .unwrap_or_default()
            );
            self.stamp = stamp;
        }
        std::rc::Rc::clone(&self.value)
    }
}
/// Ok, retry after a delay, or a permanent refusal (None) that must not be retried.
type Sent = Result<(), Option<Duration>>;
fn retry_after(response: reqwest::blocking::Response) -> Duration {
    Duration::from_secs_f64(response.json::<Value>().ok().and_then(|v| v["retry_after"].as_f64()).unwrap_or(5.0).clamp(1.0, 300.0))
}
fn credential() -> Result<String, &'static str> {
    // Only this explicitly activated process consumes the private credential file.
    // It is never discovered from the checkout or logged.
    let path = config::directory()?.join("discord.env");
    let contents = String::from_utf8(config::read_regular(&path, 16384, true)
        .map_err(|_| "discord.env must be a private regular file (mode 0600, at most 16 KiB)")?)
        .map_err(|_| "Could not load Discord configuration from discord.env.")?;
    for line in contents.lines().map(str::trim) {
        let line = line.strip_prefix("export ").unwrap_or(line);
        if let Some((name, value)) = line.split_once('=') {
            if name.trim() != "DISCORD_TOKEN" {
                continue;
            }
            let value = value.trim();
            let value = if value.len() >= 2
                && ((value.starts_with('"') && value.ends_with('"'))
                    || (value.starts_with('\'') && value.ends_with('\'')))
            {
                &value[1..value.len() - 1]
            } else {
                value
            };
            if !value.is_empty() {
                return Ok(value.into());
            }
        }
    }
    Err("Missing DISCORD_TOKEN.")
}
fn checkpoint(path: impl AsRef<Path>, bytes: &[u8]) -> Result<(), &'static str> {
    let path = path.as_ref();
    let directory = path.parent().ok_or("Invalid bridge progress path")?;
    let name = path.file_name().and_then(|n| n.to_str()).ok_or("Invalid bridge progress path")?;
    config::write_in(directory, name, bytes)
        .map_err(|_| "Could not save bridge progress.")
}
fn load_json<T: serde::de::DeserializeOwned + Default>(path: impl AsRef<Path>) -> T {
    fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}
fn design_report(question: &Value) -> Option<String> {
    let id = question["id"].as_str()?;
    if id.len() > 100 || !id.bytes().all(|b|b.is_ascii_alphanumeric() || b == b'-') { return None; }
    let title: String = question["input"]["question"].as_str()?.chars().take(500).collect();
    let options = question["input"]["options"].as_array()?;
    let mut report = format!("Reply /answer {id} NUMBER or /answer {id} YOUR_DIRECTION\n\n{title}\n");
    for (index, option) in options.iter().take(4).enumerate() {
        let label: String = option.as_str()?.chars().take(140).collect();
        let recommended = question["input"]["recommended"].as_u64() == Some(index as u64);
        report.push_str(&format!("\n{}. {}{}",index+1,label,if recommended {" (recommended)"} else {""}));
    }
    Some(report)
}
fn delivery_message(change: Option<&crate::outbox::Change>, status: &str, link: &str, report: &str) -> String {
    let prefix=format!("Control: {status}.\nContinue on this laptop: {link}\n\n");
    let edits = if status == "waiting" { None } else { change };
    let suffix=edits.map(|event|format!("\n\nChanges · {}\n{}",event.text.chars().take(100).collect::<String>(),crate::diff::discord_preview(&event.output))).unwrap_or_default();
    let budget=1800usize.saturating_sub(prefix.chars().count()+suffix.chars().count());
    let body=if report.chars().count()>budget {
        format!("{}\n… Full reply in chat.",report.chars().take(budget.saturating_sub(23)).collect::<String>())
    }else{report.to_owned()};
    format!("{prefix}{body}{suffix}")
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn dm_channel(row: &Value, user: DiscordId) -> Option<DiscordId> {
    let recipients = row["recipients"].as_array()?;
    if row["type"].as_u64() != Some(1)
        || recipients.len() != 1
        || recipients[0]["id"].as_str()?.parse::<DiscordId>().ok() != Some(user)
    {
        return None;
    }
    row["id"].as_str()?.parse().ok()
}
fn sender(row: &Value, route: &Route, bot: DiscordId) -> Option<DiscordId> {
    route.recipient?;
    if row["author"]["bot"].as_bool() == Some(true)
        || !row["webhook_id"].is_null()
        || row["channel_id"].as_str()?.parse::<DiscordId>().ok() != Some(route.channel)
    {
        return None;
    }
    if !row["guild_id"].is_null() {
        return None;
    }
    let user: DiscordId = row["author"]["id"].as_str()?.parse().ok()?;
    if user == bot {
        return None;
    }
    route.actor(user)?;
    Some(user)
}
fn rejected(row: &Value, route: &Route, bot: DiscordId) -> Option<&'static str> {
    sender(row, route, bot)?;
    discord::direct_rejection(row["content"].as_str().unwrap_or(""), row["attachments"].as_array().is_some_and(|a| !a.is_empty()))
}
fn incoming(row: &Value, route: &Route, bot: DiscordId) -> Option<discord::Incoming> {
    let user = sender(row, route, bot)?;
    let content = row["content"].as_str()?;
    let (action, objective) = if route.recipient.is_some() {
        discord::direct_command(content)?
    } else {
        discord::command(content)?
    };
    Some(discord::Incoming {
        message_id: row["id"].as_str()?.parse().ok()?,
        user_id: user,
        channel_id: route.channel,
        action,
        objective: objective.into(),
        direct: route.recipient.is_some(),
    })
}
impl Bridge {
    fn metadata(&self, path: &str) -> Result<Value, &'static str> {
        let response = self
            .client
            .get(format!("{API}{path}"))
            .send()
            .map_err(|_| "Discord metadata request failed.")?;
        if !response.status().is_success() {
            return Err("Discord denied a permission-metadata request.");
        }
        response
            .json()
            .map_err(|_| "Invalid Discord permission metadata.")
    }
    fn audit_permissions(&self) -> Result<(), &'static str> {
        let guilds = self.metadata("/users/@me/guilds?limit=200")?;
        let guilds = guilds.as_array().ok_or("Invalid bot membership metadata")?;
        let mut reports = Vec::new();
        for guild in guilds {
            let id = guild["id"]
                .as_str()
                .and_then(|s| s.parse::<DiscordId>().ok())
                .ok_or("Invalid guild ID")?;
            let member = self.metadata(&format!("/guilds/{id}/members/{}", self.bot_id))?;
            let roles = self.metadata(&format!("/guilds/{id}/roles"))?;
            let roles = roles.as_array().ok_or("Invalid role metadata")?;
            let channels = self.metadata(&format!("/guilds/{id}/channels"))?;
            let channels = channels.as_array().ok_or("Invalid channel metadata")?;
            let (mut evaluated, mut visible, mut history, mut manage, mut admin, mut unknown) =
                (0, 0, 0, 0, false, 0);
            let mut visible_types = HashMap::<u64, usize>::new();
            for channel in channels.iter().filter(|c| c["type"].as_u64() != Some(4)) {
                match discord_permissions::effective(
                    &id.to_string(),
                    &self.bot_id.to_string(),
                    &member,
                    roles,
                    channel,
                ) {
                    Ok(mut permissions) => {
                        if guild["owner"].as_bool() == Some(true) {
                            permissions = u128::MAX;
                        }
                        evaluated += 1;
                        visible += usize::from(permissions & discord_permissions::VIEW != 0);
                        if permissions & discord_permissions::VIEW != 0 {
                            *visible_types
                                .entry(
                                    channel["type"]
                                        .as_u64()
                                        .ok_or("Invalid channel type metadata")?,
                                )
                                .or_default() += 1;
                        }
                        history += usize::from(
                            permissions
                                & (discord_permissions::VIEW | discord_permissions::HISTORY)
                                == (discord_permissions::VIEW | discord_permissions::HISTORY),
                        );
                        manage += usize::from(permissions & discord_permissions::MANAGE_ROLES != 0);
                        admin |= permissions == u128::MAX;
                    }
                    Err(_) => unknown += 1,
                }
            }
            reports.push(json!({"guild_id":id,"administrator_or_owner":admin,"channels_evaluated":evaluated,"channels_with_view_permission":visible,"visible_channel_types":visible_types,"channels_with_view_and_history_permission":history,"channels_with_manage_permissions_capability":manage,"channels_with_unknown_permissions":unknown}));
        }
        let report = json!({"dm_only_bridge":true,"audit_server_message_requests_made":0,"audit_server_permission_changes_made":0,"membership_list_complete":guilds.len()<200,"servers":reports});
        checkpoint(
            config::state("discord-permission-audit.json")?,
            &serde_json::to_vec(&report).map_err(|_| "Could not encode permission audit")?,
        )?;
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|_| "Could not encode permission audit")?
        );
        Ok(())
    }
    fn diagnose(&self) -> Result<(), &'static str> {
        // Emit only selected non-secret installation metadata, never raw API bodies.
        let response = self
            .client
            .get(format!("{API}/oauth2/applications/@me"))
            .send()
            .map_err(|_| "Application metadata request failed.")?;
        if !response.status().is_success() {
            return Err("Discord denied application metadata access.");
        }
        let app: Value = response
            .json()
            .map_err(|_| "Invalid application metadata response.")?;
        let app_id = app["id"]
            .as_str()
            .and_then(|s| s.parse::<DiscordId>().ok())
            .ok_or("Invalid application ID.")?;
        let response = self
            .client
            .get(format!("{API}/users/@me/guilds"))
            .query(&[("limit", "200")])
            .send()
            .map_err(|_| "Bot server membership request failed.")?;
        if !response.status().is_success() {
            return Err("Discord denied bot server membership access.");
        }
        let guilds: Vec<Value> = response
            .json()
            .map_err(|_| "Invalid bot server membership response.")?;
        let owner = Policy::load()?
            .principals
            .into_iter()
            .find(|p| p.role == identity::Role::Architect)
            .and_then(|p| p.user_id)
            .ok_or("No architect Discord ID is configured for the membership diagnostic.")?;
        let mut shared = 0;
        let mut unknown = 0;
        for guild in &guilds {
            let Some(id) = guild["id"]
                .as_str()
                .and_then(|s| s.parse::<DiscordId>().ok())
            else {
                continue;
            };
            match self
                .client
                .get(format!("{API}/guilds/{id}/members/{owner}"))
                .send()
            {
                Ok(r) if r.status().is_success() => shared += 1,
                Ok(r) if r.status().as_u16() == 404 => {}
                _ => unknown += 1,
            }
        }
        let report = json!({"application_id":app_id,"bot_id":self.bot_id,"bot_servers_checked":guilds.len(),"membership_list_complete":guilds.len()<200,"shared_servers_with_architect":shared,"membership_checks_unknown":unknown,"bot_public":app["bot_public"].as_bool(),"bot_require_code_grant":app["bot_require_code_grant"].as_bool(),"installation_contexts":app["integration_types_config"].as_object().map(|o|o.keys().cloned().collect::<Vec<_>>()),"server_install_url":format!("https://discord.com/oauth2/authorize?client_id={app_id}&scope=bot&permissions=0")});
        checkpoint(
            config::state("discord-installation.json")?,
            &serde_json::to_vec(&report).map_err(|_| "Could not save installation diagnostic.")?,
        )?;
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|_| "Could not encode diagnostic report.")?
        );
        Ok(())
    }
    fn connect() -> Result<Self, &'static str> {
        let token = credential()?;
        let mut auth = HeaderValue::from_str(&format!("Bot {token}"))
            .map_err(|_| "Invalid Discord credential format.")?;
        auth.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, auth);
        let client = Client::builder()
            .default_headers(headers)
            .user_agent("DiscordBot (https://github.com/fennecs/control, 0.1)")
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "Could not construct Discord client.")?;
        let response = client
            .get(format!("{API}/users/@me"))
            .send()
            .map_err(|_| "Discord connection failed.")?;
        if !response.status().is_success() {
            return Err("Discord rejected the bot credential. Check DISCORD_TOKEN locally.");
        }
        let user: Value = response
            .json()
            .map_err(|_| "Invalid Discord identity response.")?;
        if user["bot"].as_bool() != Some(true) {
            return Err("The configured credential does not identify a Discord bot.");
        }
        let bot_id = user["id"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or("Invalid Discord bot ID.")?;
        Ok(Self {
            client,
            token,
            bot_id,
            bot_name: user["username"].as_str().unwrap_or("Control").into(),
            state: Default::default(),
        })
    }
    fn status(&self, routes: &[Route], ready: bool, message: &str) {
        let value = json!({"ready":ready,"updated_unix":now(),"message":message,"bot_id":self.bot_id,"bot_name":self.bot_name,"dm_users":routes.iter().filter_map(|r|r.recipient).collect::<Vec<_>>(),"transport":"REST / direct messages"});
        if let (Ok(bytes), Ok(path)) = (
            serde_json::to_vec(&value),
            config::state("discord-status.json"),
        ) {
            let _ = checkpoint(path, &bytes);
        }
    }
    fn routes(&self, previous: &[Route]) -> Result<Vec<Route>, &'static str> {
        let policy = Policy::load()?;
        let mut routes = Vec::new();
        for principal in policy.principals {
            let Some(user) = principal.user_id else {
                continue;
            };
            if let Some(route) = previous.iter().find(|r| r.recipient == Some(user)) {
                routes.push(route.clone());
                continue;
            }
            let response = self
                .client
                .post(format!("{API}/users/@me/channels"))
                .json(&json!({"recipient_id":user}))
                .send()
                .map_err(|_| "Could not open an authorized DM channel.")?;
            if response.status().as_u16() == 429 {
                return Err("Discord rate-limited DM setup; the bridge will retry.");
            }
            if !response.status().is_success() {
                eprintln!("An authorized DM channel could not be opened; check bot access and recipient privacy settings.");
                continue;
            }
            let row: Value = response
                .json()
                .map_err(|_| "Invalid DM channel response.")?;
            let channel =
                dm_channel(&row, user).ok_or("Discord returned a mismatched DM recipient.")?;
            routes.push(Route {
                channel,
                recipient: Some(user),
            });
        }
        let bindings = DmBindings {
            recipients: routes
                .iter()
                .filter_map(|r| {
                    r.recipient.map(|user_id| DmBinding {
                        user_id,
                        channel_id: r.channel,
                    })
                })
                .collect(),
        };
        checkpoint(
            config::state("discord-dm-bindings.json")?,
            &serde_json::to_vec(&bindings).map_err(|_| "Could not encode DM bindings.")?,
        )?;
        Ok(routes)
    }
    fn reply(&self, channel: DiscordId, content: &str) -> bool {
        self.send(channel, content).is_ok()
    }
    fn snapshot(&self) -> Result<std::rc::Rc<Snapshot>, &'static str> {
        Ok(self.state.borrow_mut().load(&config::state("state.json")?))
    }
    fn send(&self, channel: DiscordId, content: &str) -> Sent {
        let bindings = DmBindings::load().map_err(|_| Some(Duration::from_secs(5)))?;
        let mut authorize = identity::authorizer();
        let allowed = {
            bindings.recipients.iter().any(|r| {
                r.channel_id == channel
                    && authorize(
                        &Actor::DiscordDm {
                            user_id: r.user_id,
                            channel_id: channel,
                        },
                        Action::Task,
                    )
                    .is_ok()
            })
        };
        if !allowed {
            return Err(None);
        }
        let safe = content.replace(&self.token, "[REDACTED]");
        let content: String = safe.chars().take(1800).collect();
        match self
            .client
            .post(format!("{API}/channels/{channel}/messages"))
            .json(&json!({"content":content,"allowed_mentions":{"parse":[]}}))
            .send()
        {
            Ok(r) if r.status().is_success() => Ok(()),
            Ok(r) if r.status().as_u16() == 429 => Err(Some(retry_after(r))),
            // Other 4xx (403, 50007 cannot DM user, ...) will not succeed on retry.
            Ok(r) if r.status().is_client_error() => Err(None),
            _ => Err(Some(Duration::from_secs(5))),
        }
    }
    fn receive(
        &self,
        route: &Route,
        cursors: &mut HashMap<String, String>,
    ) -> Result<bool, &'static str> {
        let recipient = route
            .recipient
            .ok_or("Server-channel reads are disabled: this bridge is DM-only.")?;
        identity::authorize(
            &route.actor(recipient).ok_or("Invalid DM route")?,
            Action::Task,
        )?;
        let channel = route.channel.to_string();
        let endpoint = format!("{API}/channels/{channel}/messages");
        if !cursors.contains_key(&channel) {
            let response = self
                .client
                .get(&endpoint)
                .query(&[("limit", "1")])
                .send()
                .map_err(|_| "Discord channel connection failed.")?;
            if !response.status().is_success() {
                return Err("Discord denied access to a configured conversation.");
            }
            let rows: Vec<Value> = response
                .json()
                .map_err(|_| "Invalid Discord history response.")?;
            cursors.insert(
                channel.clone(),
                rows.first()
                    .and_then(|r| r["id"].as_str())
                    .unwrap_or("0")
                    .into(),
            );
            return Ok(true);
        }
        let response = self
            .client
            .get(&endpoint)
            .query(&[("limit", "100"), ("after", cursors[&channel].as_str())])
            .send()
            .map_err(|_| "Discord message read failed.")?;
        if response.status().as_u16() == 429 {
            std::thread::sleep(retry_after(response).min(Duration::from_secs(30)));
            return Ok(false);
        }
        if !response.status().is_success() {
            return Err("Discord message read rejected; check bot access.");
        }
        let mut rows: Vec<Value> = response
            .json()
            .map_err(|_| "Invalid Discord message response.")?;
        rows.sort_by_key(|r| {
            r["id"]
                .as_str()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0)
        });
        for row in rows {
            let Some(id) = row["id"].as_str().and_then(|s| s.parse::<DiscordId>().ok()) else {
                continue;
            };
            if let Some(message) = incoming(&row, route, self.bot_id) {
                let actor = message.actor();
                if identity::authorize(&actor, message.action).is_ok() {
                    let native = if message.action == Action::Improve && ["on", "off"].contains(&message.objective.as_str()) {
                        Some(format!("Improve mode change queued.\n{}", discord::chat_link(&actor).unwrap_or_default()))
                    } else if message.action == Action::Task {
                        discord::native_reply(&actor, &message.objective)
                    } else {
                        None
                    };
                    let path = config::state("discord-inbox")?.join(format!("{id}.json"));
                    // Already written or ingested before the cursor checkpoint: do not acknowledge twice.
                    let duplicate = Path::new(&path).exists() || self.snapshot()?.discord_seen.contains(&id.to_string());
                    if !duplicate {
                        checkpoint(
                            &path,
                            &serde_json::to_vec(&message)
                                .map_err(|_| "Could not encode Discord task.")?,
                        )?;
                    }
                    if duplicate && native.is_none() {
                    } else if message.objective.strip_prefix("/cancel").is_some_and(|tail| tail.is_empty() || tail.starts_with(char::is_whitespace)) {
                        let link = discord::chat_link(&actor).unwrap_or_default();
                        let _ = self.reply(route.channel, &format!("Cancellation queued.\n{link}"));
                    } else if message.objective.strip_prefix("/steer").is_some_and(|tail| tail.starts_with(char::is_whitespace)) {
                        let link = discord::chat_link(&actor).unwrap_or_default();
                        let _ = self.reply(route.channel, &format!("Queued for the next turn; the current task keeps running.\n{link}"));
                    } else if let Some(reply) = native {
                        if !self.reply(route.channel, &reply) {
                            return Ok(false);
                        }
                    } else {
                        let link = discord::chat_link(&actor).unwrap_or_default();
                        let _=self.reply(route.channel,&format!("Received.\nContinue on this laptop: {link}\nI’ll reply here when the task finishes."));
                    }
                } else if identity::authorize(&actor, Action::Task).is_ok() {
                    let _=self.reply(route.channel,"Only the architect can request Control source changes. You can still DM me a normal task in your own isolated workspace.");
                }
            } else if let Some(reason) = rejected(&row, route, self.bot_id)
                && route.recipient.and_then(|user| route.actor(user)).is_some_and(|actor| identity::authorize(&actor, Action::Task).is_ok()) {
                    let _ = self.reply(route.channel, reason);
                }
            cursors.insert(channel.clone(), id.to_string());
        }
        Ok(true)
    }
    /// One delivery attempt honouring per-receipt backoff. Permanent refusals are recorded and not retried.
    fn attempt(&self, receipt: &str, channel: DiscordId, content: &str, outbound: &mut Outbound) -> bool {
        if outbound.backoff.get(receipt).is_some_and(|(until, _)| now() < *until) {
            return false;
        }
        match self.send(channel, content) {
            Ok(()) => {
                outbound.backoff.remove(receipt);
                true
            }
            Err(Some(delay)) => {
                let failures = outbound.backoff.get(receipt).map_or(0, |(_, n)| *n) + 1;
                let wait = delay.as_secs().max(5u64 << failures.min(7)).min(600);
                outbound.backoff.insert(receipt.into(), (now() + wait, failures));
                false
            }
            Err(None) => {
                eprintln!("Discord refused a delivery permanently; it will not be retried.");
                outbound.backoff.remove(receipt);
                outbound.undelivered.insert(receipt.into());
                if let Ok(path) = config::state("discord-undelivered.json") {
                    let _ = serde_json::to_vec(&outbound.undelivered).map_err(|_| "").and_then(|b| checkpoint(path, &b));
                }
                true
            }
        }
    }
    /// Follows Control's delivery outbox from this bridge's cursor; receipts make
    /// at-least-once events exactly-once messages.
    fn deliver(&self, routes: &[Route], outbound: &mut Outbound) -> Result<(), &'static str> {
        let events = crate::outbox::read(&crate::outbox::path()?, outbound.cursor).map_err(|_| "Could not read the delivery outbox.")?;
        let mut authorize = identity::authorizer();
        // Events parked until their authorization or route is available again.
        for run in std::mem::take(&mut outbound.parked) {
            if self.deliver_one(&run, routes, outbound, &mut authorize)? != Outcome::Done {
                outbound.park(run);
            }
        }
        for (run, next) in &events {
            match self.deliver_one(run, routes, outbound, &mut authorize)? {
                Outcome::Done => {}
                Outcome::Park => outbound.park(run.clone()),
                // A transient send failure holds the cursor: retried next poll.
                Outcome::Stop => break,
            }
            outbound.cursor = *next;
        }
        Ok(())
    }
    fn deliver_one(&self, run: &crate::outbox::Delivery, routes: &[Route], outbound: &mut Outbound, authorize: &mut impl FnMut(&Actor, Action) -> Result<identity::Workspace, &'static str>) -> Result<Outcome, &'static str> {
        {
            {
                let (actor, action, status) = (&run.actor, run.action, run.status.as_str());
                let receipt = format!("{}:{status}", run.run);
                let (channel, direct) = match actor {
                    Actor::DiscordDm { channel_id, .. } => (*channel_id, true),
                    _ => return Ok(Outcome::Done),
                };
                if (!run.source.starts_with("discord:") && run.chat.is_none())
                    || !crate::outbox::DELIVERABLE.contains(&status)
                    || outbound.delivered.contains(&receipt)
                {
                    return Ok(Outcome::Done);
                }
                // Policy files or a DM route can be unavailable for a moment.
                if authorize(actor, action).is_err()
                    || !routes.iter().any(|r| r.channel == channel && r.recipient.is_some() == direct)
                {
                    return Ok(Outcome::Park);
                }
                let decision = (status == "waiting").then(||design_report(&run.question)).flatten();
                let report = if let Some(report) = decision.as_deref() {
                    report
                } else if action == Action::Improve && !direct {
                    "Architect report: open the authenticated operator website."
                } else if run.report.is_empty() {
                    "No report was produced."
                } else {
                    run.report.as_str()
                };
                let link = discord::session_link(actor, run.chat.as_deref()).unwrap_or_default();
                if !self.attempt(&receipt, channel, &delivery_message(run.change.as_ref(),status,&link,report), outbound) {
                    return Ok(Outcome::Stop);
                }
                outbound.delivered.insert(receipt);
                checkpoint(
                    config::state("discord-delivered.json")?,
                    &serde_json::to_vec(&outbound.delivered)
                        .map_err(|_| "Could not encode delivery progress.")?,
                )?;
                Ok(Outcome::Done)
            }
        }
    }
    /// Worker-queued notices (for example a DM that could not become a task).
    fn outbox(&self, routes: &[Route], outbound: &mut Outbound) -> Result<(), &'static str> {
        let directory = config::state("discord-outbox")?;
        let Ok(entries) = fs::read_dir(&directory) else { return Ok(()) };
        for entry in entries.flatten().take(64) {
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_symlink()) || path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let notice: Value = load_json(&path);
            let (Some(channel), Some(user), Some(text)) = (notice["channel_id"].as_str().and_then(|s| s.parse::<DiscordId>().ok()), notice["user_id"].as_str().and_then(|s| s.parse::<DiscordId>().ok()), notice["text"].as_str()) else {
                let _ = fs::remove_file(&path);
                continue;
            };
            if !routes.iter().any(|r| r.channel == channel && r.recipient == Some(user)) {
                continue;
            }
            let receipt = format!("outbox:{}", entry.file_name().to_string_lossy());
            if self.attempt(&receipt, channel, text, outbound) {
                let _ = fs::remove_file(&path);
            }
        }
        Ok(())
    }
}
/// What a delivery attempt left to do.
#[derive(PartialEq, Eq)]
enum Outcome {
    /// Skipped, already delivered, delivered now or refused for good.
    Done,
    /// Not deliverable right now (authorization or route unavailable): retry.
    Park,
    /// The send failed transiently: retry from this event.
    Stop,
}
#[derive(Default)]
struct Outbound {
    /// Position in the delivery outbox; starts at the beginning on launch and
    /// relies on receipts to skip what was already sent.
    cursor: crate::outbox::Cursor,
    /// Events waiting for authorization or a route; bounded, oldest dropped.
    parked: Vec<crate::outbox::Delivery>,
    delivered: HashSet<String>,
    undelivered: HashSet<String>,
    backoff: HashMap<String, (u64, u32)>,
}
impl Outbound {
    fn park(&mut self, run: crate::outbox::Delivery) {
        if self.parked.len() >= 256 {
            self.parked.remove(0);
        }
        self.parked.push(run);
    }
}
fn run(check_only: bool) -> Result<(), &'static str> {
    Policy::load()?;
    fs::create_dir_all(config::state("discord-inbox")?)
        .map_err(|_| "Could not create bridge inbox.")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            config::state_directory()?,
            fs::Permissions::from_mode(0o700),
        )
        .map_err(|_| "Could not restrict local bridge storage.")?;
    }
    let bridge = Bridge::connect()?;
    let mut routes = bridge.routes(&[])?;
    if routes.is_empty() {
        return Err("No accessible bound Discord DM recipients.");
    }
    println!(
        "Discord bot: {} / {}. DM conversations: {}.",
        bridge.bot_name,
        bridge.bot_id,
        routes.iter().filter(|r| r.recipient.is_some()).count()
    );
    if check_only {
        bridge.status(&routes, false, "Connection checked; bridge not started.");
        return Ok(());
    }
    let mut cursors: HashMap<String, String> = load_json(config::state("discord-cursors.json")?);
    let mut outbound = Outbound {
        delivered: load_json(config::state("discord-delivered.json")?),
        undelivered: load_json(config::state("discord-undelivered.json")?),
        ..Default::default()
    };
    let mut refresh = now();
    loop {
        if now().saturating_sub(refresh) >= 60 {
            match bridge.routes(&routes) {
                Ok(updated) => routes = updated,
                Err(m) => eprintln!("{m}"),
            };
            refresh = now();
        }
        let mut ready = false;
        for route in &routes {
            match bridge.receive(route, &mut cursors) {
                Ok(ok) => ready |= ok,
                Err(m) => eprintln!("{m}"),
            };
        }
        // Transient storage or Discord failures are logged; the bridge keeps polling.
        if let Err(m) = config::state("discord-cursors.json").and_then(|path| checkpoint(path, &serde_json::to_vec(&cursors).map_err(|_| "Could not encode conversation cursors.")?)) {
            eprintln!("{m}");
        }
        if let Err(m) = bridge.deliver(&routes, &mut outbound).and_then(|()| bridge.outbox(&routes, &mut outbound)) {
            eprintln!("{m}");
        }
        bridge.status(
            &routes,
            ready,
            if ready {
                "Listening for DMs from bound users."
            } else {
                "Discord read unavailable; retrying."
            },
        );
        std::thread::sleep(Duration::from_secs(3));
    }
}
pub fn command(args: &[String]) {
    if args.iter().any(|a| a == "--audit-server-permissions") {
        if let Err(m) = Bridge::connect().and_then(|bridge| bridge.audit_permissions()) {
            eprintln!("{m}");
            std::process::exit(1);
        }
        return;
    }
    if args.get(1).is_some_and(|a| a == "--channel") {
        eprintln!("Server-channel access is disabled. Control is DM-only.");
        std::process::exit(1);
    }
    if args.iter().any(|a| a == "--diagnose") {
        if let Err(m) = Bridge::connect().and_then(|bridge| bridge.diagnose()) {
            eprintln!("{m}");
            std::process::exit(1);
        }
        return;
    }
    if args
        .get(1)
        .is_some_and(|a| a == "--bind" || a == "--channel")
    {
        let result = (|| -> Result<(), &'static str> {
            let mut policy = Policy::load()?;
            if args[1] == "--bind" {
                policy.bind(
                    args.get(2).ok_or("Missing principal label")?,
                    args.get(3).ok_or("Missing Discord ID")?.parse()?,
                )?;
            } else {
                policy.channel_id = Some(args.get(2).ok_or("Missing channel ID")?.parse()?);
            }
            policy.save()
        })();
        if let Err(m) = result {
            eprintln!("{m}");
            std::process::exit(1);
        }
        println!("Discord policy updated. No .env file was read.");
        return;
    }
    let check = args.iter().any(|s| s == "--check");
    if !check && !args.iter().any(|s| s == "--enable") {
        println!("Discord bridge is off. Use --check to verify bot access or --enable to listen for DMs. No .env file was read.");
        return;
    }
    if let Err(message) = run(check) {
        eprintln!("{message}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bridge_checkpoint_preserves_existing_progress_on_temporary_file_conflict() {
        let root = std::env::temp_dir().join(format!("control-bridge-write-{}-{}",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir(&root).unwrap();
        let path = root.join("progress.json");
        checkpoint(&path, b"old").unwrap();
        let temporary = root.join(format!("progress.json.{}.tmp",std::process::id()));
        std::os::unix::fs::symlink(&path,&temporary).unwrap();
        assert!(checkpoint(&path, b"new").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"old");
        fs::remove_file(temporary).unwrap();
        checkpoint(&path, b"new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        fs::remove_dir_all(root).unwrap();
    }
    fn id(s: &str) -> DiscordId {
        s.parse().unwrap()
    }
    #[test]
    fn delivery_reserves_space_for_actual_changes_without_greening_or_losing_link() {
        let change=crate::outbox::Change{text:"1 file · +1 −1".into(),output:"@@ -1 +1 @@\n-old\n+new\n".into()};
        let message=delivery_message(Some(&change),"completed","http://localhost/chat/test",&"reply ".repeat(1000));
        assert!(message.chars().count()<=1800);
        assert!(message.contains("http://localhost/chat/test") && message.contains("```text\n"));
        assert!(message.contains("-old\n+new") && message.ends_with("```"));
        assert!(!delivery_message(Some(&change),"waiting","link","Question").contains("```"));
    }
    #[test]
    fn design_reports_keep_answer_identity_and_all_options_within_dm_budget() {
        let report = design_report(&json!({"id":"q-test","input":{"question":"q".repeat(1000),"options":["a".repeat(200),"b".repeat(200),"c".repeat(200),"d".repeat(200)],"recommended":3}})).unwrap();
        assert!(report.starts_with("Reply /answer q-test"));
        assert!(report.contains("4. ") && report.contains("(recommended)"));
        assert!(report.chars().count() < 1500);
        assert!(design_report(&json!({"id":"bad\nidentity"})).is_none());
    }
    #[test]
    fn dm_setup_checks_type_and_recipient() {
        assert_eq!(
            dm_channel(
                &json!({"id":"100","type":1,"recipients":[{"id":"2"}]}),
                id("2")
            ),
            Some(id("100"))
        );
        for row in [
            json!({"id":"100","type":3,"recipients":[{"id":"2"}]}),
            json!({"id":"100","type":1,"recipients":[{"id":"3"}]}),
        ] {
            assert!(dm_channel(&row, id("2")).is_none());
        }
    }
    #[test]
    fn direct_messages_cannot_impersonate_another_recipient() {
        let route = Route {
            channel: id("100"),
            recipient: Some(id("2")),
        };
        let mut row = json!({"id":"10","channel_id":"100","author":{"id":"2"},"content":"hello"});
        assert!(incoming(&row, &route, id("99")).unwrap().direct);
        row["author"]["id"] = json!("1");
        assert!(incoming(&row, &route, id("99")).is_none());
        row["author"]["id"] = json!("2");
        row["author"]["bot"] = json!(true);
        assert!(incoming(&row, &route, id("99")).is_none());
    }
    #[test]
    fn oversized_and_attachment_only_dms_get_a_reason() {
        let route = Route { channel: id("100"), recipient: Some(id("2")) };
        let mut row = json!({"id":"10","channel_id":"100","author":{"id":"2"},"content":"x".repeat(2001)});
        assert!(incoming(&row, &route, id("99")).is_none());
        assert!(rejected(&row, &route, id("99")).unwrap().contains("2000"));
        row["content"] = json!("");
        row["attachments"] = json!([{"id":"1"}]);
        assert!(rejected(&row, &route, id("99")).unwrap().contains("attachments"));
        row["author"]["id"] = json!("3");
        assert!(rejected(&row, &route, id("99")).is_none());
    }
    #[test]
    fn state_cache_reparses_only_after_the_file_changes() {
        let path = std::env::temp_dir().join(format!("control-bridge-cache-{}.json", std::process::id()));
        fs::write(&path, br#"{"discord_seen":["1"]}"#).unwrap();
        let mut cache = StateCache::default();
        let first = cache.load(&path);
        assert!(first.discord_seen.contains("1"));
        assert!(std::rc::Rc::ptr_eq(&first, &cache.load(&path)));
        fs::write(&path, br#"{"discord_seen":["22"]}"#).unwrap();
        assert!(cache.load(&path).discord_seen.contains("22"));
        fs::remove_file(&path).unwrap();
        assert!(cache.load(&path).discord_seen.is_empty());
    }
    #[test]
    fn state_cache_detects_atomic_replacement_and_refuses_symlinks() {
        let root = std::env::temp_dir().join(format!("control-bridge-replace-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("state.json");
        let replacement = root.join("replacement.json");
        fs::write(&path, br#"{"discord_seen":["1"]}"#).unwrap();
        let stamp = fs::metadata(&path).unwrap().modified().unwrap();
        let mut cache = StateCache::default();
        assert!(cache.load(&path).discord_seen.contains("1"));
        fs::write(&replacement, br#"{"discord_seen":["2"]}"#).unwrap();
        fs::File::options().write(true).open(&replacement).unwrap()
            .set_times(fs::FileTimes::new().set_modified(stamp)).unwrap();
        fs::rename(&replacement, &path).unwrap();
        assert!(cache.load(&path).discord_seen.contains("2"));
        fs::rename(&path, &replacement).unwrap();
        std::os::unix::fs::symlink(&replacement, &path).unwrap();
        assert!(cache.load(&path).discord_seen.is_empty());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn server_messages_are_rejected_even_with_a_prefix() {
        let route = Route {
            channel: id("100"),
            recipient: None,
        };
        let mut row = json!({"id":"10","channel_id":"100","author":{"id":"2"},"content":"hello"});
        assert!(incoming(&row, &route, id("99")).is_none());
        row["content"] = json!("!control inspect this conversation");
        assert!(incoming(&row, &route, id("99")).is_none());
        assert!(route.actor(id("2")).is_none());
    }
}
