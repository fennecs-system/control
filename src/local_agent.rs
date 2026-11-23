//! Control's native tool loop for operator-configured local endpoints.
//!
//! Control assembles the turn input, offers the attempt's worker MCP tools as
//! OpenAI-style function tools, executes every call through the same worker tool
//! dispatch the CLI adapters use (authorization, attempt fencing, handoff and
//! publication rules included), and decides when the turn ends. The endpoint only
//! generates. Provider state never outlives the attempt; recovery starts fresh.
use control_ai::{
    model::{decode_step, think, tool_request, Limits, Thinking},
    Tokens,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    io,
    time::{Duration, Instant},
};

pub(crate) type Failure = (&'static str, String);

/// Conservative token estimate for planning (about 3 UTF-8 bytes per token).
/// Real tokenizers usually do better; the server still enforces its own limit.
pub(crate) fn tokens(bytes: usize) -> u32 {
    u32::try_from(bytes.div_ceil(3)).unwrap_or(u32::MAX)
}
/// Context reserved outside the planned turn input: the role's always-loaded
/// tool schemas (under 10 KB; the rest load on demand), schemas loaded during
/// the turn, and the room the loop needs for one tool exchange before it compacts.
pub(crate) const OVERHEAD_TOKENS: u32 = CORE_TOKENS + LOADED_TOKENS + 1024;
/// Always-loaded schemas: under 10 KB for every role, about 3.4 k tokens.
const CORE_TOKENS: u32 = 4096;
/// Schemas a turn loads on demand; the loop still re-checks the actual size every step.
const LOADED_TOKENS: u32 = 2048;
pub(crate) const SYSTEM: &str = "You are an agent running inside Control on a local model. Act only through the provided tools; there is no other way to read files, run commands or reach rooms. Use control_workspace_run for short shell commands in /workspace. Call tools with arguments that are a JSON object matching the schema. Tools in the prompt's \"More tools\" index are callable by their full control_ name; Control loads the schema on first use. When the request is complete or you are waiting on someone, call control_turn_handoff if it is offered; otherwise reply with a brief plain-text final report and no tool call. Write each key fact or diagnosis you establish on its own line starting Finding: and each choice on a line starting Decision:; Control keeps those lines verbatim when it summarizes older steps, and control_history_read with a query searches the original older steps.";
const RESULT_BYTES: usize = 12000;
const CALLS_PER_STEP: usize = 8;
/// Share of the input budget below which a turn's requests are append-only:
/// older steps stay whole, so each request extends the previous one byte for
/// byte and the server's prefix cache reuses all of it. Shrinking rewrites the
/// middle of the prompt, which costs a re-prefill of everything after it; below
/// this share that costs more prefill time than the context it saves.
const SHRINK_PERCENT: u32 = 50;
/// Share of the input budget at which a turn compacts by summary, before it is
/// exhausted, so a summary call still has room to succeed.
const COMPACT_PERCENT: u32 = 75;
/// Share of the input budget at which the agent is told, once, to hand off.
const NOTE_PERCENT: u32 = 80;
/// Model steps left in the turn at which the agent is told, once, to hand off
/// (a quarter of the cap when that is fewer).
const STEP_NOTE_LEFT: usize = 8;

mod context;
mod thinking;

pub(crate) struct Policy {
    /// Model steps before the turn stops with a host checkpoint
    /// (`local_agent_steps` in execution.json).
    pub max_steps: usize,
    /// Consecutive malformed steps (bad JSON, unknown tool, empty or cut reply)
    /// answered with a corrective message before the attempt fails visibly.
    pub max_repairs: usize,
    /// The newest steps a shrink keeps verbatim; older steps are then seen
    /// through the lens (shrunk, then folded only when over the threshold).
    pub verbatim_steps: usize,
    /// A shrink covers at least this many steps, so the request prefix stays
    /// byte-identical across the steps between shrinks (prefix-cache reuse).
    /// Steps also leave the recent window for recall in batches of this many.
    pub shrink_batch: usize,
    /// Share of the input budget a request must reach before older steps are
    /// shrunk (`SHRINK_PERCENT`); below it every request is append-only.
    pub shrink_percent: u32,
    /// The model entry's base thinking level; each step adapts around it.
    pub thinking: Thinking,
    /// The turn resumes a cut-off one (host checkpoint or retried attempt):
    /// minimal thinking until its first tool call.
    pub resumed: bool,
}
impl Default for Policy {
    fn default() -> Self {
        Self { max_steps: 64, max_repairs: 3, verbatim_steps: 4, shrink_batch: 4, shrink_percent: SHRINK_PERCENT, thinking: Thinking::default(), resumed: false }
    }
}
/// The in-turn compactor's limits: its input budget in bytes (system prompt
/// excluded) and its output ceiling, which sizes the requested summary.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Compactor {
    pub input_bytes: usize,
    pub output_tokens: u32,
}

pub(crate) enum Poll {
    Continue,
    /// The attempt committed a handoff; the summary is the result.
    Finish(String),
    Stop(Failure),
}
/// Effects the loop needs. The worker binds these to the live attempt; tests
/// bind them to a mock endpoint and a recording dispatcher.
pub(crate) trait Host {
    fn tools(&mut self) -> io::Result<Vec<Value>>;
    /// A worker MCP `tools/call` result (`content`, `isError`).
    fn call(&mut self, name: &str, arguments: &Value, nonce: &str) -> Value;
    fn poll(&mut self) -> Poll;
    fn model(&mut self, body: &Value) -> io::Result<Value>;
    /// One bounded compaction call on the chat's local compactor, asking for a
    /// summary under `words` words.
    fn summarize(&mut self, evidence: &str, words: usize) -> io::Result<String>;
    fn record(&mut self, kind: &str, id: &str, text: &str, output: &str, status: &str);
    /// The turn reached its model-step cap without a final report or handoff;
    /// the worker then writes a host checkpoint for the next turn.
    fn step_limit(&mut self) {}
    /// The turn's usage so far, after every model call (answered or not).
    fn metered(&mut self, meter: &Meter);
    /// The chat's declared observation for the next request, refreshed when
    /// stale without blocking on a slow probe; `first` is the turn's first step.
    fn observation(&mut self, _first: bool) -> Option<String> {
        None
    }
    /// New conversation callbacks for the end of the next request, recorded
    /// as delivered so each appears once.
    fn inbox(&mut self) -> Option<String> {
        None
    }
    /// The reasoning of the step just answered, kept by the host only to
    /// carry into a host checkpoint if the turn is cut off; never transcript.
    fn reasoned(&mut self, _reasoning: &str) {}
    /// Keep a block of this turn's steps as a node of the conversation's
    /// summary tree, so `control_history_read` can read and search it:
    /// `text` is its summary (a leaf step's shrunk rendering), and a node has
    /// either two retained `children` or one original source per step.
    /// Returns the node's key; `None` when it was not kept.
    fn retain(&mut self, _block: control_comonad::Block, _text: &str, _children: Option<[String; 2]>, _sources: Vec<String>) -> Option<String> {
        None
    }
}

/// Per-call usage summed over the turn. Every call counts, including one that
/// failed or was cancelled in flight, so a missing report is never zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Meter {
    pub tokens: Option<Tokens>,
    /// Model calls sent.
    pub calls: usize,
    /// Calls whose response reported usage.
    pub reported: usize,
    /// Reasoning tokens within the output tokens: as reported, else estimated
    /// from the returned reasoning text.
    pub reasoning: u64,
}
impl Meter {
    fn add(&mut self, tokens: Option<Tokens>) {
        if let Some(t) = tokens {
            self.tokens.get_or_insert_with(Tokens::default).add(t);
            self.reported += 1;
        }
    }
    /// Reported once the turn is over if every call reported; while it runs,
    /// or when some call did not report, the sum is a lower bound.
    pub fn usage(&self, over: bool, model: Option<String>) -> control_ai::Usage {
        control_ai::Usage { tokens: self.tokens, complete: over && self.reported == self.calls, model }
    }
}

pub(crate) struct Outcome {
    pub text: String,
    pub tokens: Option<Tokens>,
    pub steps: usize,
    pub compactions: usize,
    pub elapsed: Duration,
}

/// Keeps a tool description's opening sentences within a small byte budget:
/// schemas are re-sent every step and compete with history in a local context.
fn brief(description: &str) -> String {
    const LIMIT: usize = 320;
    if description.len() <= LIMIT {
        return description.to_owned();
    }
    let mut end = LIMIT;
    while !description.is_char_boundary(end) {
        end -= 1;
    }
    let cut = &description[..end];
    match cut.rfind(". ") {
        Some(at) if at > 80 => cut[..=at].to_owned(),
        _ => format!("{}…", cut.trim_end()),
    }
}
/// MCP tool definitions become chat-completions function tools with unchanged
/// names and parameter schemas; long descriptions keep their opening sentences.
pub(crate) fn functions(definitions: &[Value]) -> Vec<Value> {
    definitions
        .iter()
        .filter_map(|tool| {
            let name = tool["name"].as_str()?;
            Some(json!({"type":"function","function":{"name":name,
                "description":brief(tool["description"].as_str().unwrap_or("")),
                "parameters":if tool["inputSchema"].is_object() {tool["inputSchema"].clone()} else {json!({"type":"object","properties":{}})}}}))
        })
        .collect()
}
const SEARCH: &str = crate::worker_tools::deferred::SEARCH;
/// Schemas a successful `control_tool_search` returned become native
/// function tools for the rest of this turn. Calls still go through the
/// worker dispatch, which alone decides whether they may run.
fn load(result: &Value, tools: &mut Vec<Value>, names: &mut BTreeSet<String>) {
    if result["isError"] == true {
        return;
    }
    let found = result["structuredContent"]["data"]["tools"].as_array().cloned().unwrap_or_default();
    for tool in functions(&found) {
        if let Some(name) = tool["function"]["name"].as_str() && names.insert(name.to_owned()) {
            tools.push(tool);
        }
    }
}
/// A search result as the model reads it: every found schema is now one of
/// the request's function tools, so each is named with its first sentence.
fn searched(mut result: Value) -> Value {
    let Some(data) = result.get_mut("structuredContent").and_then(|s| s.get_mut("data")) else { return result };
    let Some(found) = data.get("tools").and_then(Value::as_array).filter(|t| !t.is_empty()) else { return result };
    let lines = found.iter().map(|tool| {
        let text = tool["description"].as_str().unwrap_or("");
        let mut end = text.find(". ").map_or(text.len(), |at| at + 1).min(160);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        json!({"name":tool["name"],"description":&text[..end]})
    }).collect::<Vec<_>>();
    data["tools"] = json!(lines);
    let text = data.to_string();
    // The first item is the data; host notes follow it unchanged.
    if result["content"][0]["type"] == "text" {
        result["content"][0]["text"] = json!(text);
    }
    result
}
fn result_text(result: &Value) -> (String, bool) {
    let failed = result["isError"] == true || result.is_null();
    let mut text: String = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if result.is_null() {
        text = "No result was returned.".into();
    }
    if text.len() > RESULT_BYTES {
        let mut end = RESULT_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n[truncated by Control; request a narrower result]");
    }
    (if failed { format!("ERROR: {text}") } else { text }, failed)
}
/// Appends a host note to the newest tool result, or as a user message.
fn append_note(messages: &mut Vec<Value>, note: &str) {
    match messages.last_mut() {
        Some(last) if last["role"] == "tool" => {
            let text = format!("{}\n\n{note}", last["content"].as_str().unwrap_or(""));
            last["content"] = Value::String(text);
        }
        _ => messages.push(json!({"role":"user","content":note})),
    }
}
fn size(messages: &[Value]) -> usize {
    serde_json::to_vec(messages).map_or(usize::MAX, |v| v.len())
}

pub(crate) fn run(
    host: &mut dyn Host,
    model: &str,
    limits: &Limits,
    compactor: Compactor,
    prompt: &str,
    policy: &Policy,
) -> Result<Outcome, Failure> {
    let started = Instant::now();
    let failed = |m: String| ("failed", m);
    let definitions = host.tools().map_err(|e| failed(format!("Could not list worker tools: {e}")))?;
    let mut tools = functions(&definitions);
    let mut names: BTreeSet<String> = tools
        .iter()
        .filter_map(|t| t["function"]["name"].as_str().map(str::to_owned))
        .collect();
    let mut tool_tokens = tokens(size(&tools));
    let budget = limits.context_tokens.saturating_sub(limits.output_tokens);
    let system = if policy.thinking == Thinking::Off { SYSTEM.to_owned() } else { format!("{SYSTEM}{}", thinking::NOTE) };
    let mut messages = vec![
        json!({"role":"system","content":system}),
        json!({"role":"user","content":prompt}),
    ];
    // Admission: the mandatory input must fit before any model call.
    if tool_tokens.saturating_add(tokens(size(&messages))) > budget {
        return Err(("blocked", format!(
            "Turn input does not fit the local model: about {} tokens of instructions and history plus {tool_tokens} of tool schemas exceed {budget} available ({} context − {} output). Increase the declared context or reduce history; nothing was sent.",
            tokens(size(&messages)), limits.context_tokens, limits.output_tokens)));
    }
    let mut meter = Meter::default();
    let (mut steps, mut repairs, mut compactions) = (0usize, 0usize, 0usize);
    let (mut noted, mut step_noted) = (false, false);
    let mut lens = context::Lens::default();
    // Oldest steps shrunk so far; it only grows, so the shrunk prefix is stable.
    let mut shrunk = 0usize;
    let mut adaptive = thinking::Adaptive::new(policy.thinking, policy.resumed);
    // A `/think` request already granted for the step being re-sent.
    let mut granted = false;
    loop {
        match host.poll() {
            Poll::Continue => {}
            Poll::Finish(summary) => {
                return Ok(Outcome { text: summary, tokens: meter.tokens, steps, compactions, elapsed: started.elapsed() });
            }
            Poll::Stop(failure) => return Err(failure),
        }
        if steps >= policy.max_steps {
            host.step_limit();
            return Err(failed(format!("Local agent reached its {steps}-step limit without a final report or handoff. The published checkpoint is unchanged; Control recorded this turn's last tool calls and workspace as a host checkpoint for the next turn to resume from (local_agent_steps in execution.json sets the limit).")));
        }
        // The declared observation, refreshed for this step. It ends the
        // request only: never stored in the steps, so it is never part of the
        // stable prefix, never shrunk and never folded.
        let observation = host.observation(steps == 0).map(|text| json!({"role":"user","content":text}));
        // New conversation callbacks join the newest, unsent step once, so the
        // sent prefix is unchanged and later requests keep them in history.
        if let Some(block) = host.inbox() {
            host.record("note", &format!("local-inbox-{steps}"), &block, "", "completed");
            append_note(&mut messages, &block);
        }
        let fixed = tool_tokens.saturating_add(observation.as_ref().map_or(0, |o| tokens(size(std::slice::from_ref(o)))));
        // Context fit, re-checked every step: the request is the lens view of
        // the turn's original steps. Past the threshold, the oldest blocks are
        // folded on the local compactor (never dropped silently) before the
        // context is full; folds persist, so later prefixes stay stable.
        let used = |view: &[Value]| fixed.saturating_add(tokens(size(view)));
        let percent = |view: &[Value]| u32::try_from(u64::from(used(view)) * 100 / u64::from(budget.max(1))).unwrap_or(u32::MAX);
        // Below `shrink_percent` the request is append-only. Reaching it, every
        // step but the newest `verbatim_steps` shrinks at once (at least
        // `shrink_batch` of them), and the shrunk prefix then stays fixed while
        // the request grows back towards the threshold.
        let count = context::steps(&messages).len();
        let mut view = lens.view(&messages, count - shrunk, false);
        let target = count.saturating_sub(policy.verbatim_steps);
        if percent(&view) >= policy.shrink_percent && target >= shrunk + policy.shrink_batch.max(1) {
            shrunk = target;
            view = lens.view(&messages, count - shrunk, false);
        }
        let mut verbatim = count - shrunk;
        let mut shrink_all = false;
        let reached = percent(&view);
        let mut folded = false;
        if reached >= COMPACT_PERCENT {
            let mut problem = None;
            while percent(&view) >= COMPACT_PERCENT {
                match lens.unfolded(count, verbatim) {
                    Some(block) => match lens.fold(host, block, &messages, compactor) {
                        Ok(done) => {
                            folded = true;
                            for how in done {
                                compactions += 1;
                                host.record("compaction", &format!("local-compaction-{compactions}"), &how, "", "completed");
                            }
                        }
                        Err(error) => {
                            problem = Some(error);
                            break;
                        }
                    },
                    // Everything older is folded: bring the next newest step under the lens.
                    None if verbatim > 1 => verbatim -= 1,
                    None => break,
                }
                view = lens.view(&messages, verbatim, false);
            }
            // Last resort before failing: shrink the verbatim steps too, the newest included.
            if used(&view) > budget {
                shrink_all = true;
                view = lens.view(&messages, verbatim, true);
                host.record("compaction", &format!("local-compaction-{}", compactions + 1), "Shortened every bulky tool result of this turn to fit the context", "", "completed");
            }
            if used(&view) > budget {
                let reason = problem.map_or_else(|| "nothing older remains".to_owned(), |e| e.to_string());
                return Err(failed(format!(
                    "Local model context is exhausted ({budget} tokens) and this turn could not be compacted further: {reason}. Hand off sooner or raise the declared context.")));
            }
        }
        // Once per turn: the turn has filled most of its context (or lost
        // detail to a fold); ask for a checkpoint before it runs out.
        let note = crate::worker_tools::nudges::context_text(reached, names.contains("control_turn_handoff"));
        if !noted && (reached >= NOTE_PERCENT || folded) && used(&view).saturating_add(tokens(note.len() + 16)) <= budget {
            noted = true;
            host.record("note", "local-context-note", &note, "", "completed");
            append_note(&mut messages, &note);
            view = lens.view(&messages, verbatim, shrink_all);
        }
        // Once per turn: few model steps remain before the cap stops the turn.
        let left = policy.max_steps - steps;
        if !step_noted && steps > 0 && left <= STEP_NOTE_LEFT.min(policy.max_steps / 4) {
            step_noted = true;
            let note = crate::worker_tools::nudges::steps_text(left, names.contains("control_turn_handoff"));
            host.record("note", "local-step-note", &note, "", "completed");
            append_note(&mut messages, &note);
            view = lens.view(&messages, verbatim, shrink_all);
        }
        // Steps that left the recent window, or are no longer seen verbatim,
        // join the summary tree for recall, whether or not they were shrunk.
        let recent = context::window(count, policy.verbatim_steps, policy.shrink_batch).min(verbatim);
        lens.retain_shrunk(host, &messages, if shrink_all { count } else { count - recent });
        view.extend(observation);
        let mut body = tool_request(model, &view, &tools, limits).map_err(|e| failed(e.to_string()))?;
        // Thinking shares `max_tokens` with the answer: the budget is at most
        // half of it, so a tool call always has the other half.
        let level = adaptive.level(folded);
        think(&mut body, level.budget(limits.output_tokens));
        meter.calls += 1;
        let value = match host.model(&body) {
            Ok(value) => value,
            Err(error) => {
                host.metered(&meter);
                if let Poll::Stop(failure) = host.poll() {
                    return Err(failure);
                }
                return Err(failed(format!("Local model call failed: {error}. No hosted fallback was used; the published checkpoint is unchanged.")));
            }
        };
        meter.add(control_ai::model::usage(&value));
        let decoded = decode_step(&value, limits);
        let reasoning = decoded.as_ref().ok().and_then(|s| s.reasoning.as_deref());
        meter.reasoning += control_ai::model::reasoning_tokens(&value).unwrap_or_else(|| reasoning.map_or(0, |r| u64::from(tokens(r.len()))));
        host.metered(&meter);
        if let Some(reasoning) = reasoning {
            host.reasoned(reasoning);
        }
        let step = decoded.map_err(|e| failed(format!("Local model returned an unusable response: {e}")))?;
        steps += 1;
        let asked_more = step.calls.is_empty() && step.text.as_deref().is_some_and(thinking::asks_more);
        if !asked_more {
            granted = false;
        }
        let repair = |repairs: &mut usize, messages: &mut Vec<Value>, note: String| -> Result<(), Failure> {
            *repairs += 1;
            if *repairs > policy.max_repairs {
                return Err(("failed", format!("Local model produced {} malformed steps in a row; last problem: {note} The published checkpoint is unchanged.", *repairs)));
            }
            messages.push(json!({"role":"user","content":format!("Control could not use your last reply: {note}")}));
            Ok(())
        };
        if step.truncated {
            // The answer gets the room next time.
            adaptive.after(thinking::Cue::Cut);
            repair(&mut repairs, &mut messages, format!("it reached the {}-token output limit and was discarded. Reply more briefly, or make one tool call.", limits.output_tokens))?;
            continue;
        }
        if asked_more {
            // `/think` alone: re-send the same step with more thinking, once.
            // The transcript is unchanged, so the whole prefix is reused.
            if !granted && level < Thinking::High && adaptive.switch("/think").is_some() {
                granted = true;
                continue;
            }
            adaptive.after(thinking::Cue::Trouble);
            repair(&mut repairs, &mut messages, "it asked to think longer, but this step already had the most thinking it can get. Call a tool or give your final report.".into())?;
            continue;
        }
        if step.calls.is_empty() {
            match step.text {
                Some(text) => {
                    host.record("message", &format!("local-step-{steps}"), &text, "", "completed");
                    return Ok(Outcome { text, tokens: meter.tokens, steps, compactions, elapsed: started.elapsed() });
                }
                None => {
                    adaptive.after(thinking::Cue::Trouble);
                    repair(&mut repairs, &mut messages, "it contained neither text nor a tool call. Call a tool or give your final report.".into())?;
                    continue;
                }
            }
        }
        if let Some(text) = &step.text {
            host.record("message", &format!("local-step-{steps}"), text, "", "completed");
        }
        // Only the reply and its calls enter the transcript, never the reasoning.
        let mut trouble = false;
        let calls: Vec<_> = step.calls.into_iter().enumerate().map(|(i, c)| {
            let id = c.id.clone().unwrap_or_else(|| format!("call-{steps}-{i}"));
            (id, c)
        }).collect();
        messages.push(json!({"role":"assistant","content":step.text,
            "tool_calls":calls.iter().map(|(id,c)|json!({"id":id,"type":"function","function":{"name":c.name,
                "arguments":c.arguments.as_ref().map_or_else(|_|"{}".to_owned(),|a|a.to_string())}})).collect::<Vec<_>>()}));
        // Marked lines of the reasoning outlive it: compaction carries them.
        if let Some(reasoning) = step.reasoning.as_deref() {
            lens.reasoned(context::steps(&messages).len() - 1, reasoning);
        }
        let mut malformed = None;
        for (index, (id, call)) in calls.iter().enumerate() {
            // A deferred tool called by name loads its schema first, through
            // the same search the model could call; nothing else is admitted.
            if index < CALLS_PER_STEP && !names.contains(&call.name) && call.arguments.is_ok() && names.contains(SEARCH) {
                let found = host.call(SEARCH, &json!({"names":[call.name]}), &format!("{steps}-{index}-1"));
                load(&found, &mut tools, &mut names);
            }
            let content = if index >= CALLS_PER_STEP {
                malformed = Some(format!("more than {CALLS_PER_STEP} tool calls in one step."));
                format!("ERROR: not executed; at most {CALLS_PER_STEP} tool calls run per step.")
            } else if !names.contains(&call.name) {
                malformed = Some(format!("unknown tool {:?}.", call.name));
                format!("ERROR: unknown tool {:?}; nothing ran. Use one of the offered tools.", call.name)
            } else {
                match &call.arguments {
                    Err(problem) => {
                        malformed = Some(format!("{} {problem}.", call.name));
                        format!("ERROR: {problem}; nothing ran. Retry with a JSON object matching the tool schema.")
                    }
                    Ok(arguments) => {
                        // Digits and dashes: room posts key idempotency on the run and this nonce.
                        let nonce = format!("{steps}-{index}");
                        host.record("mcp_tool_call", &format!("local-{nonce}"), &call.name, "", "in_progress");
                        let mut result = host.call(&call.name, arguments, &nonce);
                        if call.name == SEARCH && result["isError"] != true {
                            load(&result, &mut tools, &mut names);
                            result = searched(result);
                        }
                        let (text, error) = result_text(&result);
                        host.record("mcp_tool_call", &format!("local-{nonce}"), &call.name, &text, if error { "failed" } else { "completed" });
                        adaptive.acted();
                        trouble |= error;
                        context::without_repeats(text, arguments)
                    }
                }
            };
            messages.push(json!({"role":"tool","tool_call_id":id,"content":content}));
        }
        // Loaded schemas stay offered for the rest of the turn and count against its context.
        tool_tokens = tokens(size(&tools));
        // A failed call (a refused handoff included) or a malformed one earns
        // more thinking next step; a routine success less. A soft switch in
        // the reply overrides that for the next step.
        adaptive.after(if trouble || malformed.is_some() { thinking::Cue::Trouble } else { thinking::Cue::Routine });
        if let Some(text) = &step.text {
            adaptive.switch(text);
        }
        match malformed {
            Some(note) => repair(&mut repairs, &mut messages, note)?,
            None => repairs = 0,
        }
    }
}

use crate::State;
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

/// The live attempt's effects. Tools go through the same worker dispatch and
/// fencing as the CLI mailboxes; nothing here grants additional authority.
struct Live<'a> {
    shared: &'a Arc<Mutex<State>>,
    run: &'a str,
    generation: String,
    workspace: &'a Path,
    managed: bool,
    attempt: &'a mut crate::usage::Attempt,
    endpoint: crate::model_calls::Endpoint,
    compactor: crate::model_calls::Endpoint,
    started: Instant,
    admitted: u64,
    served: Option<control_ai::model::Served>,
    timed_out: bool,
    meter: Meter,
    /// Attempt start in nanoseconds; prefixes request nonces.
    stamp: u128,
    /// The latest step's reasoning, carried into a host checkpoint if the turn
    /// is cut off.
    reasoning: Option<String>,
}
impl Live<'_> {
    fn bridge(&self) -> Option<(&Arc<Mutex<State>>, &str)> {
        Some((self.shared, self.run))
    }
    fn rpc(&self, request: Value, nonce: &str) -> Value {
        crate::worker_tools::local_rpc(self.shared, self.run, &json!({"nonce":nonce,"request":request}), self.workspace, self.managed)
    }
    /// What the server says it is: its `model`, else its runtime fingerprint.
    fn reported_model(&self) -> Option<String> {
        self.served.as_ref().and_then(|s| s.model.clone().or_else(|| s.fingerprint.clone()))
    }
}
impl Host for Live<'_> {
    fn tools(&mut self) -> io::Result<Vec<Value>> {
        let response = self.rpc(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}), &format!("{}-0", self.stamp));
        response["result"]["tools"].as_array().cloned().ok_or_else(|| {
            io::Error::other(response["error"]["message"].as_str().unwrap_or("tool listing refused").to_owned())
        })
    }
    fn call(&mut self, name: &str, arguments: &Value, nonce: &str) -> Value {
        // Unique across every attempt of the run: a numeric attempt stamp prefix.
        let nonce = format!("{}-{nonce}", self.stamp);
        let response = self.rpc(json!({"jsonrpc":"2.0","id":nonce,"method":"tools/call","params":{"name":name,"arguments":arguments}}), &nonce);
        match response.get("result") {
            Some(result) => result.clone(),
            None => json!({"content":[{"type":"text","text":response["error"]["message"].as_str().unwrap_or("Tool call refused")}],"isError":true}),
        }
    }
    fn poll(&mut self) -> Poll {
        if crate::design::requested(self.bridge()) {
            return Poll::Stop(("waiting", "Waiting for a design decision.".into()));
        }
        if crate::lifecycle::cancelled(self.bridge()) {
            return Poll::Stop(("cancelled", "Cancelled. The previous published workspace is unchanged.".into()));
        }
        let elapsed = self.started.elapsed() + Duration::from_secs(self.admitted);
        match crate::attempt_control::poll(self.bridge(), self.attempt, elapsed) {
            crate::attempt_control::Decision::Continue => Poll::Continue,
            crate::attempt_control::Decision::Finish(summary) => Poll::Finish(summary),
            crate::attempt_control::Decision::TimedOut(seconds) => {
                self.timed_out = true;
                self.attempt.termination = Some(crate::attempt_control::Termination::TimedOut);
                Poll::Stop(("failed", crate::worker::timeout_message(Duration::from_secs(seconds))))
            }
        }
    }
    fn model(&mut self, body: &Value) -> io::Result<Value> {
        let (shared, run) = (self.shared, self.run);
        let cancel = || crate::lifecycle::cancelled(Some((shared, run))) || crate::design::requested(Some((shared, run)));
        let (value, served) = crate::model_calls::post_json(&self.endpoint, body, &cancel)?;
        self.served.get_or_insert(served);
        Ok(value)
    }
    fn summarize(&mut self, evidence: &str, words: usize) -> io::Result<String> {
        crate::model_calls::call(&self.compactor, &crate::compaction::system_within(words), evidence).map(|r| r.text)
    }
    fn record(&mut self, kind: &str, id: &str, text: &str, output: &str, status: &str) {
        if let Ok(mut state) = self.shared.lock() {
            crate::worker::record_activity_for(&mut state, self.run, &self.generation, crate::usage::Activity {
                id: id.chars().take(100).collect(),
                kind: kind.into(),
                text: text.chars().take(4000).collect(),
                output: output.chars().take(4000).collect(),
                status: status.into(),
            });
        }
    }
    fn step_limit(&mut self) {
        self.attempt.termination = Some(crate::attempt_control::Termination::StepLimit);
    }
    fn observation(&mut self, first: bool) -> Option<String> {
        crate::observe::step(self.shared, self.run, first)
    }
    fn inbox(&mut self) -> Option<String> {
        let mut state = self.shared.lock().ok().filter(|_| self.managed)?;
        crate::turn_context::inbox(&mut state, self.run, crate::storage::save)
    }
    fn reasoned(&mut self, reasoning: &str) {
        self.reasoning = Some(crate::turn_context::carried_reasoning(reasoning));
    }
    fn retain(&mut self, block: control_comonad::Block, text: &str, children: Option<[String; 2]>, sources: Vec<String>) -> Option<String> {
        // Recall is best effort: a failed write leaves the step unsearchable,
        // never the turn failed; the record says so.
        match crate::compaction::turn::retain(self.shared, self.run, &self.generation, &self.compactor, block, text, children, sources) {
            Ok(key) => Some(key),
            Err(error) => {
                self.record("compaction", "local-recall", &format!("Could not keep steps {}–{} for recall: {error}", block.start + 1, block.end()), "", "failed");
                None
            }
        }
    }
    fn metered(&mut self, meter: &Meter) {
        self.meter = *meter;
        // Live totals: one in-memory update per model call, never a save.
        let usage = meter.usage(false, self.reported_model());
        if let Ok(mut state) = self.shared.lock() {
            crate::worker::record_usage_for(&mut state, self.run, &self.generation, usage);
        }
    }
}

/// Run one attempt on a local endpoint. The caller has prepared and fenced the
/// workspace; it quiesces command jobs and publishes after a successful return.
pub(crate) fn execute(
    attempt: &mut crate::usage::Attempt,
    bridge: Option<(&Arc<Mutex<State>>, &str)>,
    workspace: &Path,
    prompt: &str,
) -> Result<String, Failure> {
    let (shared, run) = bridge.ok_or(("blocked", "Local agent turns need a live Control node.".to_owned()))?;
    let (endpoint, compactor, managed, resumed) = {
        let state = shared.lock().map_err(|_| ("failed", "Control state unavailable".to_owned()))?;
        let model = crate::providers::Model::captured(crate::backend::Backend::Local, attempt.model.clone());
        if !crate::providers::agent_capable(&state.runtime, &model) {
            return Err(("blocked", crate::backend::LOCAL_TEXT_ONLY.into()));
        }
        let endpoint = crate::providers::endpoint(&state.runtime, &attempt.model).cloned()
            .ok_or(("blocked", "The selected local endpoint is no longer configured; no fallback was used.".to_owned()))?;
        let current = state.runtime.runs.iter().find(|r| r.id == run);
        let session = current.and_then(|r| r.chat_id.as_ref()).and_then(|chat| state.chats.iter().find(|c| &c.id == chat));
        // Resuming a cut-off turn: the chat holds a host checkpoint, or this
        // run already had an attempt (the live one is its last).
        let resumed = session.and_then(|s| s.continuity.as_ref()).is_some_and(|c| c.interrupted.is_some())
            || current.is_some_and(|r| r.attempts.len() > 1);
        // A local chat compacts on its own endpoint unless the operator chose otherwise.
        let compactor = session.and_then(|s| crate::compaction::compactor_for(&state, s)).cloned().unwrap_or_else(|| endpoint.clone());
        // Same transport choice as the CLI mailboxes: managed turns get nudges.
        let managed = attempt.context_trace.is_some();
        (endpoint, compactor, managed, resumed)
    };
    let generation = attempt.workspace_id.clone().ok_or(("failed", "Missing attempt identity".to_owned()))?;
    let admitted = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default().as_secs().saturating_sub(attempt.started_unix);
    let limits = endpoint.limits.clone();
    let budget = Compactor {
        input_bytes: compactor.limits.input_budget().saturating_sub(crate::compaction::SYSTEM.len() + crate::compaction::TARGET_BYTES),
        output_tokens: compactor.limits.output_tokens,
    };
    let mut live = Live {
        shared, run, generation, workspace, managed, attempt, endpoint, compactor,
        started: Instant::now(), admitted, served: None, timed_out: false, meter: Meter::default(),
        stamp: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos(),
        reasoning: None,
    };
    let (model, thinking) = (live.endpoint.model.clone(), live.endpoint.thinking);
    let result = run_loop(&mut live, &model, &limits, budget, prompt, thinking, resumed);
    let (meter, reported_model) = (live.meter, live.reported_model());
    let served = live.served.take().unwrap_or_default();
    let provenance = live.endpoint.provenance(&served);
    let reasoning = live.reasoning.take();
    let attempt = live.attempt;
    // Cut off (time limit, step cap, provider failure): the latest reasoning
    // goes into the host checkpoint the worker writes for such a failure, so
    // the next turn continues that plan. The worker drops it otherwise.
    if result.is_err() {
        attempt.interrupted_reasoning = reasoning;
    }
    // Whatever the ending, keep what the calls reported: exact when every sent
    // call reported (also after a failure or a cancel between steps), a lower
    // bound when one did not (cancelled in flight, no usage). No call sent
    // (refused admission, cancelled first) stays unrecorded: not launched.
    if meter.calls > 0 {
        attempt.record_usage(meter.usage(true, reported_model));
    }
    if let Ok(outcome) = &result {
        {
            let seconds = outcome.elapsed.as_secs_f64().max(0.001);
            let summary = format!("{} · {} model steps · {} compactions · {:.1} s{}", provenance, outcome.steps, outcome.compactions, seconds,
                outcome.tokens.map(|t| format!(" · {} in ({} cached) / {} out tokens{} · {:.1} output tokens/s end-to-end", t.input_tokens, t.cached_input_tokens, t.output_tokens,
                    if meter.reasoning > 0 { format!(" ({} reasoning)", meter.reasoning) } else { String::new() }, t.output_tokens as f64 / seconds)).unwrap_or_else(|| " · usage not reported".into()));
            if let Ok(mut state) = shared.lock() {
                let generation = attempt.workspace_id.clone().unwrap_or_default();
                crate::worker::record_activity_for(&mut state, run, &generation, crate::usage::Activity {
                    id: "local-model".into(), kind: "local_model".into(), text: summary, output: String::new(), status: "completed".into(),
                });
            }
        }
    }
    // The loop has ended: stop attempt-owned tool containers, as the CLI adapters
    // do when their harness exits. Persistent services are separate resources.
    crate::attempt_control::stop(Some((shared, run)), attempt).map_err(|m| ("interrupted", m.to_owned()))?;
    result.map(|outcome| outcome.text)
}
fn run_loop(live: &mut Live<'_>, model: &str, limits: &Limits, budget: Compactor, prompt: &str, thinking: Thinking, resumed: bool) -> Result<Outcome, Failure> {
    let max_steps = live.shared.lock().map_or(Policy::default().max_steps, |state| state.runtime.execution.local_agent_steps);
    run(live, model, limits, budget, prompt, &Policy { max_steps, thinking, resumed, ..Policy::default() })
}

/// An operator-configured private endpoint. Control runs the tool loop itself;
/// the CLI backends run their own.
pub(crate) struct Local;
impl crate::backend::Agent for Local {
    fn id(&self) -> &'static str {
        "local"
    }
    fn label(&self) -> &'static str {
        "Local endpoint"
    }
    fn name(&self) -> &'static str {
        "Local"
    }
    fn default_model(&self) -> String {
        String::new()
    }
    fn default_capable(&self) -> bool {
        false
    }
    /// The Codex worker image, used only for credential-free workspace commands.
    fn image(&self) -> Option<String> {
        crate::runtime::worker_image()
    }
    fn image_hint(&self) -> &'static str {
        NO_IMAGE
    }
    fn credentials_ready(&self) -> Result<(), String> {
        Ok(())
    }
    fn probe(&self, _image: &str) -> Result<(), String> {
        Err(NO_IMAGE.to_owned())
    }
    fn text_command(&self, _call: &crate::backend::TextCall<'_>) -> std::io::Result<std::process::Command> {
        Err(std::io::Error::other(NOT_A_CLI))
    }
    fn text(&self, _reader: &mut dyn std::io::Read, _max: usize, _emit: &mut dyn FnMut(control_ai::Event)) -> std::io::Result<control_ai::Output> {
        Err(std::io::Error::other(NOT_A_CLI))
    }
    fn host_shell(&self) -> bool {
        true
    }
}
const NO_IMAGE: &str = "local endpoints run no worker image";
const NOT_A_CLI: &str = "Local endpoints are not CLI adapters";

#[cfg(test)]
mod tests;
