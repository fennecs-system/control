//! The relay's bounds as pure values: how much one attempt may send through
//! it, and which audit lines retention drops. The edge supplies the clock.

/// Milliseconds since the attempt's relay started.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Millis(pub u64);

const MINUTE: u64 = 60_000;

/// One attempt's bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Requests in flight at once.
    pub streams: u8,
    /// Requests started in one minute.
    pub per_minute: u16,
    /// Request and response bytes over the whole attempt.
    pub total_bytes: u64,
}
impl Limits {
    pub const DEFAULT: Self = Self {
        streams: 8,
        per_minute: 120,
        total_bytes: 4 << 30,
    };
}

/// Why a request is refused for the attempt's budget (`429`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Deny {
    Streams,
    Rate,
    Bytes,
}
impl Deny {
    pub fn name(self) -> &'static str {
        match self {
            Self::Streams => "too_many_streams",
            Self::Rate => "rate_limited",
            Self::Bytes => "byte_budget_spent",
        }
    }
}

/// One admitted request: its share of the attempt's bytes. Released once.
#[derive(Debug, PartialEq, Eq)]
pub struct Ticket {
    allowance: u64,
}
impl Ticket {
    /// Response bytes it may still carry.
    pub fn allowance(&self) -> u64 {
        self.allowance
    }
}

/// What an attempt has used. Each step returns the next value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    limits: Limits,
    active: u8,
    window: Millis,
    started: u16,
    used: u64,
}
impl Budget {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits,
            active: 0,
            window: Millis(0),
            started: 0,
            used: 0,
        }
    }
    /// Admits a request of `request` bytes (head and body) at `now`, or says why not.
    pub fn admit(&self, now: Millis, request: u64) -> Result<(Self, Ticket), Deny> {
        let (window, started) = if now.0.saturating_sub(self.window.0) >= MINUTE {
            (now, 0)
        } else {
            (self.window, self.started)
        };
        let used = self.used.saturating_add(request);
        if self.active >= self.limits.streams {
            return Err(Deny::Streams);
        }
        if started >= self.limits.per_minute {
            return Err(Deny::Rate);
        }
        if used >= self.limits.total_bytes {
            return Err(Deny::Bytes);
        }
        let next = Self {
            active: self.active + 1,
            window,
            started: started + 1,
            used,
            ..*self
        };
        let ticket = Ticket {
            allowance: self.limits.total_bytes - used,
        };
        Ok((next, ticket))
    }
    /// Ends `ticket`'s request, which carried `response` bytes back.
    pub fn release(&self, ticket: Ticket, response: u64) -> Self {
        let Ticket { .. } = ticket;
        Self {
            active: self.active.saturating_sub(1),
            used: self.used.saturating_add(response),
            ..*self
        }
    }
}

/// How long and how large `secrets-audit.jsonl` may grow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retention {
    pub max_age_seconds: u64,
    pub max_bytes: u64,
}
impl Retention {
    pub const DEFAULT: Self = Self {
        max_age_seconds: 30 * 24 * 60 * 60,
        max_bytes: 4 << 20,
    };
}
/// What pruning knows of one audit line already in the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Line {
    /// Unix seconds; none when the line cannot be read.
    pub time: Option<u64>,
    /// With its newline.
    pub bytes: u64,
}
/// The positions of `lines` to drop before appending `incoming` bytes at
/// `now`: unreadable lines, lines older than the retention age, then the
/// oldest until the log fits its size.
pub fn prune(lines: &[Line], now: u64, incoming: u64, retention: Retention) -> Vec<usize> {
    let cutoff = now.saturating_sub(retention.max_age_seconds);
    let fresh = |line: &Line| line.time.is_some_and(|time| time >= cutoff);
    let kept: u64 = lines.iter().filter(|l| fresh(l)).map(|l| l.bytes).sum();
    let excess = (kept + incoming).saturating_sub(retention.max_bytes);
    lines
        .iter()
        .enumerate()
        .scan(excess, |excess, (at, line)| {
            let drop = !fresh(line) || *excess > 0;
            if fresh(line) {
                *excess = excess.saturating_sub(line.bytes);
            }
            Some((at, drop))
        })
        .filter_map(|(at, drop)| drop.then_some(at))
        .collect()
}
