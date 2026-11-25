//! Control's native tool loop for operator-configured local endpoints.
//!
//! Control assembles the turn input, offers the attempt's worker MCP tools as
//! OpenAI-style function tools, executes every call through the same worker tool
//! dispatch the CLI adapters use (authorization, attempt fencing, handoff and
//! publication rules included), and decides when the turn ends. The endpoint only
//! generates. Provider state never outlives the attempt; recovery starts fresh.
//!
//! What the loop asks of the world is [`Effects`]. In a turn the only binding
//! is `crate::live`, over the shared `control_agent::Host` every backend gets;
//! tests bind it to a scripted endpoint and a recording dispatcher.
use crate::{Meter, SYSTEM, append_note, functions, load, result_text, searched, size, thinking, tokens};
use control_agent::{Compactor, Failure};
use control_ai::{
    Tokens,
    model::{Limits, Thinking, decode_step, think, tool_request},
};
use control_context::nudges;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    io,
    time::{Duration, Instant},
};

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

pub struct Policy {
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
        Self {
            max_steps: 64,
            max_repairs: 3,
            verbatim_steps: 4,
            shrink_batch: 4,
            shrink_percent: SHRINK_PERCENT,
            thinking: Thinking::default(),
            resumed: false,
        }
    }
}
pub enum Poll {
    Continue,
    /// The attempt committed a handoff; the summary is the result.
    Finish(String),
    Stop(Failure),
}
/// Effects the loop needs. A turn binds these to the shared host
/// (`crate::live`); tests bind them to a mock endpoint and a recording dispatcher.
pub trait Effects {
    fn tools(&mut self) -> io::Result<Vec<Value>>;
    /// A worker MCP `tools/call` result (`content`, `isError`).
    fn call(&mut self, name: &str, arguments: &Value, nonce: &str) -> Value;
    fn poll(&mut self) -> Poll;
    fn model(&mut self, body: &Value) -> io::Result<Value>;
    /// One bounded compaction call on the chat's local compactor, asking for a
    /// summary under `words` words.
    fn summarize(&mut self, evidence: &str, words: usize) -> io::Result<String>;
    fn record(&mut self, kind: &str, id: &str, text: &str, output: &str, status: &str);
    /// Whether another turn follows a handoff: the chat is a persistent bot.
    fn continues(&mut self) -> bool {
        true
    }
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
    fn retain(
        &mut self,
        _block: control_comonad::Block,
        _text: &str,
        _children: Option<[String; 2]>,
        _sources: Vec<String>,
    ) -> Option<String> {
        None
    }
}

pub struct Outcome {
    pub text: String,
    pub tokens: Option<Tokens>,
    pub steps: usize,
    pub compactions: usize,
    pub elapsed: Duration,
}

const SEARCH: &str = control_context::TOOL_SEARCH;

pub fn run<E: Effects + ?Sized>(
    host: &mut E,
    model: &str,
    limits: &Limits,
    compactor: Compactor,
    prompt: &str,
    policy: &Policy,
) -> Result<Outcome, Failure> {
    let started = Instant::now();
    let failed = Failure::Failed;
    let definitions = host
        .tools()
        .map_err(|e| failed(format!("Could not list worker tools: {e}")))?;
    let mut tools = functions(&definitions);
    let mut names: BTreeSet<String> = tools
        .iter()
        .filter_map(|t| t["function"]["name"].as_str().map(str::to_owned))
        .collect();
    let mut tool_tokens = tokens(size(&tools));
    let budget = limits.context_tokens.saturating_sub(limits.output_tokens);
    let system = if policy.thinking == Thinking::Off {
        SYSTEM.to_owned()
    } else {
        format!("{SYSTEM}{}", thinking::NOTE)
    };
    let mut messages = vec![
        json!({"role":"system","content":system}),
        json!({"role":"user","content":prompt}),
    ];
    // Admission: the mandatory input must fit before any model call.
    if tool_tokens.saturating_add(tokens(size(&messages))) > budget {
        return Err(Failure::Blocked(format!(
            "Turn input does not fit the local model: about {} tokens of instructions and history plus {tool_tokens} of tool schemas exceed {budget} available ({} context − {} output). Increase the declared context or reduce history; nothing was sent.",
            tokens(size(&messages)),
            limits.context_tokens,
            limits.output_tokens
        )));
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
                return Ok(Outcome {
                    text: summary,
                    tokens: meter.tokens,
                    steps,
                    compactions,
                    elapsed: started.elapsed(),
                });
            }
            Poll::Stop(failure) => return Err(failure),
        }
        if steps >= policy.max_steps {
            return Err(Failure::StepLimit(steps));
        }
        // The declared observation, refreshed for this step. It ends the
        // request only: never stored in the steps, so it is never part of the
        // stable prefix, never shrunk and never folded.
        let observation = host
            .observation(steps == 0)
            .map(|text| json!({"role":"user","content":text}));
        // New conversation callbacks join the newest, unsent step once, so the
        // sent prefix is unchanged and later requests keep them in history.
        if let Some(block) = host.inbox() {
            host.record("note", &format!("local-inbox-{steps}"), &block, "", "completed");
            append_note(&mut messages, &block);
        }
        let fixed = tool_tokens.saturating_add(
            observation
                .as_ref()
                .map_or(0, |o| tokens(size(std::slice::from_ref(o)))),
        );
        // Context fit, re-checked every step: the request is the lens view of
        // the turn's original steps. Past the threshold, the oldest blocks are
        // folded on the local compactor (never dropped silently) before the
        // context is full; folds persist, so later prefixes stay stable.
        let used = |view: &[Value]| fixed.saturating_add(tokens(size(view)));
        let percent =
            |view: &[Value]| u32::try_from(u64::from(used(view)) * 100 / u64::from(budget.max(1))).unwrap_or(u32::MAX);
        // Below `shrink_percent` the request is append-only. Reaching it, every
        // step but the newest `verbatim_steps` shrinks at once (at least
        // `shrink_batch` of them), and the shrunk prefix then stays fixed while
        // the request grows back towards the threshold.
        let count = context::steps(&messages).len();
        let mut view = lens.view(&messages, count - shrunk, false);
        let target = count.saturating_sub(policy.verbatim_steps);
        if percent(&view) >= policy.shrink_percent && target >= shrunk + policy.shrink_batch.max(1) {
            if shrunk == 0 {
                let note = nudges::elision_text(policy.verbatim_steps);
                host.record("note", "local-elision-note", &note, "", "completed");
                append_note(&mut messages, &note);
            }
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
                                host.record(
                                    "compaction",
                                    &format!("local-compaction-{compactions}"),
                                    &how,
                                    "",
                                    "completed",
                                );
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
                host.record(
                    "compaction",
                    &format!("local-compaction-{}", compactions + 1),
                    "Shortened every bulky tool result of this turn to fit the context",
                    "",
                    "completed",
                );
            }
            if used(&view) > budget {
                let reason = problem.map_or_else(|| "nothing older remains".to_owned(), |e| e.to_string());
                return Err(failed(format!(
                    "Local model context is exhausted ({budget} tokens) and this turn could not be compacted further: {reason}. Hand off sooner or raise the declared context."
                )));
            }
        }
        // Once per turn: the turn has filled most of its context (or lost
        // detail to a fold); ask for a checkpoint before it runs out.
        let next = nudges::Next::of(
            names.contains("control_turn_handoff"),
            host.continues(),
            names.contains("control_bot_request"),
        );
        let note = nudges::context_text(reached, next);
        if !noted
            && (reached >= NOTE_PERCENT || folded)
            && used(&view).saturating_add(tokens(note.len() + 16)) <= budget
        {
            noted = true;
            host.record("note", "local-context-note", &note, "", "completed");
            append_note(&mut messages, &note);
            view = lens.view(&messages, verbatim, shrink_all);
        }
        // Once per turn: few model steps remain before the cap stops the turn.
        let left = policy.max_steps - steps;
        if !step_noted && steps > 0 && left <= STEP_NOTE_LEFT.min(policy.max_steps / 4) {
            step_noted = true;
            let note = nudges::steps_text(left, next);
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
                return Err(failed(format!(
                    "Local model call failed: {error}. No hosted fallback was used; the published checkpoint is unchanged."
                )));
            }
        };
        meter.add(control_ai::model::usage(&value));
        let decoded = decode_step(&value, limits);
        let reasoning = decoded.as_ref().ok().and_then(|s| s.reasoning.as_deref());
        meter.reasoning += control_ai::model::reasoning_tokens(&value)
            .unwrap_or_else(|| reasoning.map_or(0, |r| u64::from(tokens(r.len()))));
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
                return Err(Failure::Failed(format!(
                    "Local model produced {} malformed steps in a row; last problem: {note} The published checkpoint is unchanged.",
                    *repairs
                )));
            }
            messages.push(json!({"role":"user","content":format!("Control could not use your last reply: {note}")}));
            Ok(())
        };
        if step.truncated {
            // The answer gets the room next time.
            adaptive.after(thinking::Cue::Cut);
            repair(
                &mut repairs,
                &mut messages,
                format!(
                    "it reached the {}-token output limit and was discarded. Reply more briefly, or make one tool call.",
                    limits.output_tokens
                ),
            )?;
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
                    return Ok(Outcome {
                        text,
                        tokens: meter.tokens,
                        steps,
                        compactions,
                        elapsed: started.elapsed(),
                    });
                }
                None => {
                    adaptive.after(thinking::Cue::Trouble);
                    repair(
                        &mut repairs,
                        &mut messages,
                        "it contained neither text nor a tool call. Call a tool or give your final report.".into(),
                    )?;
                    continue;
                }
            }
        }
        if let Some(text) = &step.text {
            host.record("message", &format!("local-step-{steps}"), text, "", "completed");
        }
        // Only the reply and its calls enter the transcript, never the reasoning.
        let mut trouble = false;
        let calls: Vec<_> = step
            .calls
            .into_iter()
            .enumerate()
            .map(|(i, c)| {
                let id = c.id.clone().unwrap_or_else(|| format!("call-{steps}-{i}"));
                (id, c)
            })
            .collect();
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
            if index < CALLS_PER_STEP && !names.contains(&call.name) && call.arguments.is_ok() && names.contains(SEARCH)
            {
                let found = host.call(SEARCH, &json!({"names":[call.name]}), &format!("{steps}-{index}-1"));
                load(&found, &mut tools, &mut names);
            }
            let content = if index >= CALLS_PER_STEP {
                malformed = Some(format!("more than {CALLS_PER_STEP} tool calls in one step."));
                format!("ERROR: not executed; at most {CALLS_PER_STEP} tool calls run per step.")
            } else if !names.contains(&call.name) {
                malformed = Some(format!("unknown tool {:?}.", call.name));
                format!(
                    "ERROR: unknown tool {:?}; nothing ran. Use one of the offered tools.",
                    call.name
                )
            } else {
                match &call.arguments {
                    Err(problem) => {
                        malformed = Some(format!("{} {problem}.", call.name));
                        format!("ERROR: {problem}; nothing ran. Retry with a JSON object matching the tool schema.")
                    }
                    Ok(arguments) => {
                        // Digits and dashes: room posts key idempotency on the run and this nonce.
                        let nonce = format!("{steps}-{index}");
                        host.record(
                            "mcp_tool_call",
                            &format!("local-{nonce}"),
                            &call.name,
                            "",
                            "in_progress",
                        );
                        let mut result = host.call(&call.name, arguments, &nonce);
                        if call.name == SEARCH && result["isError"] != true {
                            load(&result, &mut tools, &mut names);
                            result = searched(result);
                        }
                        let (text, error) = result_text(&result);
                        host.record(
                            "mcp_tool_call",
                            &format!("local-{nonce}"),
                            &call.name,
                            &text,
                            if error { "failed" } else { "completed" },
                        );
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
        adaptive.after(if trouble || malformed.is_some() {
            thinking::Cue::Trouble
        } else {
            thinking::Cue::Routine
        });
        if let Some(text) = &step.text {
            adaptive.switch(text);
        }
        match malformed {
            Some(note) => repair(&mut repairs, &mut messages, note)?,
            None => repairs = 0,
        }
    }
}

#[cfg(test)]
mod tests;
