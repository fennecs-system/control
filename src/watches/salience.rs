//! Salience filters: the perception layer between a routine's notifications
//! and the agent. A filter is declared once at `control_watch_start` and is
//! pure data: rules of stages, evaluated on each notification payload. Every
//! notification stays in the spool and remains readable; a filter only decides
//! whether it wakes. Control does not know what a field means.
//!
//! An event wakes when any rule passes. Stages run left to right and each sees
//! only events that passed the stages before it. Signal stages (`rate`,
//! `zscore`, `hysteresis`) update from every event that reaches them; delivery
//! stages (`change`, `rate_limit`) compare with the last *woken* event of the
//! same key, shared by all rules. `debounce` holds the latest event and wakes
//! with it after a quiet period, running the stages after it at release time;
//! an event that fails the stages before it cancels the hold (the change
//! reverted), and a wake supersedes that key's holds. Events that no rule can read (a missing
//! or non-numeric field in every rule) follow `unmatched` (default `wake`), so
//! completion and failure notices without the fields still reach the agent.
//!
//! State is bounded: at most 8 keys (least recently seen evicted), 64 window
//! samples per key, short previews and long values stored as digests.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::VecDeque;

const MAX_RULES: usize = 4;
const MAX_STAGES: usize = 6;
const MAX_KEYS: usize = 8;
const MAX_SAMPLES: usize = 64;
const RATE_SAMPLES: usize = 16;
const MIN_ZSCORE_SAMPLES: usize = 5;
const MAX_SECONDS: f64 = 7.0 * 86_400.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    /// JSON pointer whose value partitions all state, e.g. one baseline per symbol.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub rules: Vec<Vec<Stage>>,
    #[serde(default)]
    pub unmatched: Unmatched,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unmatched {
    #[default]
    Wake,
    Retain,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Stage {
    /// Passes when the number is at least / at most a bound (severity passthrough).
    Threshold { field: String, #[serde(default, skip_serializing_if = "Option::is_none")] at_least: Option<f64>, #[serde(default, skip_serializing_if = "Option::is_none")] at_most: Option<f64> },
    /// Passes on a change from the last woken value: by percent or absolute
    /// amount for numbers, or any difference (dedup) when neither is given.
    Change { field: String, #[serde(default, skip_serializing_if = "Option::is_none")] percent: Option<f64>, #[serde(default, skip_serializing_if = "Option::is_none")] absolute: Option<f64> },
    /// Passes when |Δvalue| per second over the window is at least `at_least`.
    Rate { field: String, seconds: f64, at_least: f64 },
    /// Passes when the value is `at_least` standard deviations from the mean of
    /// the previous `samples` values (after five).
    Zscore { field: String, samples: usize, at_least: f64 },
    /// Fires once on reaching `trigger`, then re-arms only after the value has
    /// stayed past `clear` for `clear_seconds`. `trigger < clear` watches a low side.
    Hysteresis { field: String, trigger: f64, clear: f64, #[serde(default)] clear_seconds: f64 },
    /// Holds the latest event; it wakes after `seconds` without another one.
    Debounce { seconds: f64 },
    /// At most one wake per `seconds` for this key.
    RateLimit { seconds: f64 },
}
impl Stage {
    fn field(&self) -> Option<&str> {
        match self {
            Self::Threshold { field, .. } | Self::Change { field, .. } | Self::Rate { field, .. } | Self::Zscore { field, .. } | Self::Hysteresis { field, .. } => Some(field),
            Self::Debounce { .. } | Self::RateLimit { .. } => None,
        }
    }
    fn samples(&self) -> usize {
        match self { Self::Rate { .. } => RATE_SAMPLES, Self::Zscore { samples, .. } => *samples, _ => 0 }
    }
}

fn pointer(field: &str) -> Result<(), String> {
    if field.len() > 128 || !(field.is_empty() || field.starts_with('/')) || field.chars().any(char::is_control) {
        return Err(format!("{field:?} is not a JSON pointer such as /price (at most 128 bytes)"));
    }
    Ok(())
}
fn seconds(value: f64, zero: bool) -> Result<(), String> {
    if !(value.is_finite() && value <= MAX_SECONDS && (value > 0.0 || zero && value == 0.0)) {
        return Err("Durations are seconds greater than 0 and at most 604800".into());
    }
    Ok(())
}
fn positive(value: Option<f64>) -> Result<(), String> {
    if value.is_some_and(|v| !(v.is_finite() && v > 0.0)) { return Err("Thresholds for change, rate and zscore must be positive".into()); }
    Ok(())
}

impl Filter {
    pub fn validate(&self) -> Result<(), String> {
        if self.rules.is_empty() || self.rules.len() > MAX_RULES || self.rules.iter().any(|r| r.is_empty() || r.len() > MAX_STAGES) {
            return Err(format!("salience needs 1–{MAX_RULES} rules of 1–{MAX_STAGES} stages"));
        }
        if let Some(key) = &self.key { pointer(key)?; }
        for stage in self.rules.iter().flatten() {
            if let Some(field) = stage.field() { pointer(field)?; }
            match stage {
                Stage::Threshold { at_least, at_most, .. } => {
                    if at_least.is_none() && at_most.is_none() || [at_least, at_most].into_iter().flatten().any(|v| !v.is_finite()) {
                        return Err("threshold needs a finite at_least or at_most".into());
                    }
                }
                Stage::Change { percent, absolute, .. } => { positive(*percent)?; positive(*absolute)?; }
                Stage::Rate { seconds: window, at_least, .. } => { seconds(*window, false)?; positive(Some(*at_least))?; }
                Stage::Zscore { samples, at_least, .. } => {
                    if !(MIN_ZSCORE_SAMPLES..=MAX_SAMPLES).contains(samples) { return Err(format!("zscore samples must be {MIN_ZSCORE_SAMPLES}–{MAX_SAMPLES}")); }
                    positive(Some(*at_least))?;
                }
                Stage::Hysteresis { trigger, clear, clear_seconds, .. } => {
                    if !(trigger.is_finite() && clear.is_finite()) || trigger == clear { return Err("hysteresis needs distinct finite trigger and clear values".into()); }
                    seconds(*clear_seconds, true)?;
                }
                Stage::Debounce { seconds: s } | Stage::RateLimit { seconds: s } => seconds(*s, false)?,
            }
        }
        if self.rules.iter().flatten().map(Stage::samples).sum::<usize>() > MAX_SAMPLES {
            return Err(format!("rate (16 each) and zscore stages may keep at most {MAX_SAMPLES} samples together"));
        }
        Ok(())
    }
    /// Distinct pointers, the key first; values are extracted once per event.
    fn fields(&self) -> Vec<&str> {
        let mut fields = Vec::with_capacity(4);
        for field in self.key.as_deref().into_iter().chain(self.rules.iter().flatten().filter_map(Stage::field)) {
            if !fields.contains(&field) { fields.push(field); }
        }
        fields
    }
}

/// A value as compared by `change`: a number, or bounded text (long text and
/// structured values are compared by digest).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
enum Last {
    Number(f64),
    Text(String),
}
impl Last {
    fn of(value: &Value) -> Self {
        match value {
            Value::Number(n) => n.as_f64().map_or_else(|| Self::text(&n.to_string()), Self::Number),
            Value::String(s) => Self::text(s),
            other => Self::text(&other.to_string()),
        }
    }
    fn text(text: &str) -> Self {
        if text.len() <= 64 { return Self::Text(text.into()); }
        let digest = ring::digest::digest(&ring::digest::SHA256, text.as_bytes());
        Self::Text(digest.as_ref()[..16].iter().fold(String::from("sha256:"), |mut s, b| { s.push_str(&format!("{b:02x}")); s }))
    }
    fn number(&self) -> Option<f64> { match self { Self::Number(n) => Some(*n), Self::Text(_) => None } }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct StageState {
    #[serde(default, skip_serializing_if = "VecDeque::is_empty")]
    samples: VecDeque<(u64, f64)>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    disarmed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    clear_since: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Held {
    rule: usize,
    /// First stage still to run at release.
    stage: usize,
    seq: u64,
    due: u64,
    values: Vec<Option<Last>>,
    preview: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Partition {
    key: String,
    touched: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    woke_at: Option<u64>,
    /// Values of the last woken event, indexed like `Filter::fields`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    woken: Vec<Option<Last>>,
    /// One per stage across all rules, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    stages: Vec<StageState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    held: Vec<Held>,
}
impl Partition {
    fn wake(&mut self, values: &[Option<Last>], now: u64) {
        self.woke_at = Some(now);
        self.woken.resize(values.len(), None);
        for (slot, value) in self.woken.iter_mut().zip(values) {
            if value.is_some() { slot.clone_from(value); }
        }
        self.held.clear();
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Counters {
    /// Notifications that woke the agent (including released holds).
    pub woke: u64,
    /// Notifications that did not wake on arrival; still readable as events.
    pub retained: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Salience {
    pub filter: Filter,
    #[serde(default)]
    pub counters: Counters,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    partitions: Vec<Partition>,
}

enum Outcome { Pass, Block, Missing, Held }

fn millis(seconds: f64) -> u64 { (seconds * 1000.0) as u64 }

impl Salience {
    pub fn new(filter: Filter) -> Self { Self { filter, counters: Counters::default(), partitions: Vec::new() } }
    pub fn holding(&self) -> usize { self.partitions.iter().map(|p| p.held.len()).sum() }
    /// Wake counts for event reads; `summary` adds the filter for status.
    pub fn counts(&self) -> Value {
        serde_json::json!({"woke":self.counters.woke,"retained":self.counters.retained,"holding":self.holding()})
    }
    pub fn summary(&self) -> Value {
        let mut summary = self.counts();
        summary["filter"] = serde_json::json!(self.filter);
        summary
    }

    /// Whether one notification wakes the agent. Call `release` first so a
    /// hold that fell due before this event is not superseded by it.
    pub fn admits(&mut self, seq: u64, payload: &[u8], now: u64) -> bool {
        let Self { filter, partitions, counters } = self;
        let parsed = serde_json::from_slice::<Value>(payload).ok();
        let fields = filter.fields();
        let values = fields.iter().map(|f| parsed.as_ref().and_then(|v| lookup(v, f)).map(Last::of)).collect::<Vec<_>>();
        let key = match filter.key.as_ref().and(values.first()) {
            Some(Some(Last::Text(text))) => text.clone(),
            Some(Some(Last::Number(n))) => n.to_string(),
            _ => String::new(),
        };
        let partition = partition(partitions, key, now, filter.rules.iter().map(Vec::len).sum());
        let preview = || String::from_utf8_lossy(payload).chars().take(200).collect::<String>();
        let (mut pass, mut readable, mut base) = (false, false, 0);
        for (rule, stages) in filter.rules.iter().enumerate() {
            // Every rule sees the event so signal stages keep complete windows.
            match run(stages, rule, base, 0, &fields, &values, partition, seq, now, &preview) {
                Outcome::Pass => { pass = true; readable = true; }
                Outcome::Block | Outcome::Held => readable = true,
                Outcome::Missing => {}
            }
            base += stages.len();
        }
        if pass {
            partition.wake(&values, now);
        } else if readable || filter.unmatched == Unmatched::Retain {
            counters.retained += 1;
            return false;
        }
        counters.woke += 1;
        true
    }

    /// Releases holds due by `now`, passing each woken (seq, preview) to `wake`.
    pub fn release(&mut self, now: u64, mut wake: impl FnMut(u64, &str)) {
        let Self { filter, partitions, counters } = self;
        let fields = filter.fields();
        for partition in partitions.iter_mut() {
            while let Some(position) = partition.held.iter().position(|h| h.due <= now) {
                let held = partition.held.remove(position);
                let base = filter.rules[..held.rule].iter().map(Vec::len).sum();
                let preview = || held.preview.clone();
                if let Outcome::Pass = run(&filter.rules[held.rule], held.rule, base, held.stage, &fields, &held.values, partition, held.seq, now, &preview) {
                    partition.wake(&held.values, now);
                    counters.woke += 1;
                    wake(held.seq, &held.preview);
                }
            }
        }
    }
}

/// `Value::pointer` without its per-token allocation when nothing is escaped.
fn lookup<'a>(value: &'a Value, pointer: &str) -> Option<&'a Value> {
    if pointer.contains('~') { return value.pointer(pointer); }
    pointer.split('/').skip(1).try_fold(value, |value, token| match value {
        Value::Object(map) => map.get(token),
        Value::Array(items) if !token.starts_with('+') && (token == "0" || !token.starts_with('0')) => token.parse::<usize>().ok().and_then(|index| items.get(index)),
        _ => None,
    })
}

fn partition(partitions: &mut Vec<Partition>, key: String, now: u64, stages: usize) -> &mut Partition {
    let index = match partitions.iter().position(|p| p.key == key) {
        Some(index) => index,
        None => {
            if partitions.len() == MAX_KEYS {
                // Evict the least recently seen key, sparing pending holds.
                let oldest = (0..MAX_KEYS).min_by_key(|&i| (!partitions[i].held.is_empty(), partitions[i].touched)).unwrap_or(0);
                partitions.swap_remove(oldest);
            }
            partitions.push(Partition { key, ..Partition::default() });
            partitions.len() - 1
        }
    };
    let partition = &mut partitions[index];
    partition.touched = now;
    partition.stages.resize_with(stages, StageState::default);
    partition
}

#[allow(clippy::too_many_arguments)]
fn run(stages: &[Stage], rule: usize, base: usize, from: usize, fields: &[&str], values: &[Option<Last>], partition: &mut Partition, seq: u64, now: u64, preview: &dyn Fn() -> String) -> Outcome {
    let slot = |field: &str| fields.iter().position(|f| *f == field).unwrap_or(0);
    for (index, stage) in stages.iter().enumerate().skip(from) {
        let current = stage.field().and_then(|field| values.get(slot(field))).and_then(Option::as_ref);
        let number = current.and_then(Last::number);
        if stage.field().is_some() && (current.is_none() || number.is_none() && !matches!(stage, Stage::Change { percent: None, absolute: None, .. })) {
            return Outcome::Missing;
        }
        let v = number.unwrap_or_default();
        let state = &mut partition.stages[base + index];
        let pass = match stage {
            Stage::Threshold { at_least, at_most, .. } => at_least.is_some_and(|a| v >= a) || at_most.is_some_and(|b| v <= b),
            Stage::Change { field, percent, absolute } => match partition.woken.get(slot(field)).and_then(Option::as_ref) {
                None => true,
                Some(last) if percent.is_none() && absolute.is_none() => Some(last) != current,
                Some(last) => {
                    let Some(last) = last.number() else { return Outcome::Missing };
                    let delta = (v - last).abs();
                    absolute.is_some_and(|a| delta >= a) || percent.is_some_and(|p| if last == 0.0 { delta > 0.0 } else { delta / last.abs() * 100.0 >= p })
                }
            },
            Stage::Rate { seconds, at_least, .. } => {
                let window = millis(*seconds);
                while state.samples.front().is_some_and(|(t, _)| now.saturating_sub(*t) > window) { state.samples.pop_front(); }
                let pass = state.samples.front().is_some_and(|&(t, x)| now > t && (v - x).abs() * 1000.0 / (now - t) as f64 >= *at_least);
                // Decimate so 16 samples span the whole window at any event rate.
                if state.samples.back().is_none_or(|(t, _)| now.saturating_sub(*t) >= window / RATE_SAMPLES as u64) {
                    if state.samples.len() == RATE_SAMPLES { state.samples.pop_front(); }
                    state.samples.push_back((now, v));
                }
                pass
            }
            Stage::Zscore { samples, at_least, .. } => {
                let n = state.samples.len();
                let pass = n >= MIN_ZSCORE_SAMPLES && {
                    let mean = state.samples.iter().map(|s| s.1).sum::<f64>() / n as f64;
                    let deviation = (state.samples.iter().map(|s| (s.1 - mean).powi(2)).sum::<f64>() / n as f64).sqrt();
                    if deviation == 0.0 { v != mean } else { (v - mean).abs() / deviation >= *at_least }
                };
                if n >= *samples { state.samples.pop_front(); }
                state.samples.push_back((now, v));
                pass
            }
            Stage::Hysteresis { trigger, clear, clear_seconds, .. } => {
                let high = trigger > clear;
                let (triggered, cleared) = if high { (v >= *trigger, v <= *clear) } else { (v <= *trigger, v >= *clear) };
                if state.disarmed {
                    if cleared { state.clear_since.get_or_insert(now); }
                    // The clear condition held from clear_since until at least this sample.
                    if state.clear_since.is_some_and(|t| now.saturating_sub(t) >= millis(*clear_seconds)) {
                        (state.disarmed, state.clear_since) = (false, None);
                    } else if !cleared {
                        state.clear_since = None;
                    }
                }
                let fire = !state.disarmed && triggered;
                if fire { (state.disarmed, state.clear_since) = (true, None); }
                fire
            }
            Stage::Debounce { seconds } => {
                partition.held.retain(|h| h.rule != rule);
                partition.held.push(Held { rule, stage: index + 1, seq, due: now.saturating_add(millis(*seconds)), values: values.to_vec(), preview: preview() });
                return Outcome::Held;
            }
            Stage::RateLimit { seconds } => partition.woke_at.is_none_or(|t| now.saturating_sub(t) >= millis(*seconds)),
        };
        if !pass {
            // A live event failing the stages before a debounce means the
            // condition reverted within the quiet period: drop that hold.
            if from == 0 { partition.held.retain(|h| h.rule != rule); }
            return Outcome::Block;
        }
    }
    Outcome::Pass
}

#[cfg(test)]
mod tests;
