//! Typed GitHub operations shared with the host broker. No credentials.
use serde::{Deserialize, Serialize};

pub const USAGE: &str = "/github PR [overview|files|comments|reviews|review-comments|checks|statuses] [PAGE]\n/github PR comment|title|body TEXT\n/github PR merge HEAD_SHA [squash|merge|rebase]";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MergeMethod {
    #[default]
    Squash,
    Merge,
    Rebase,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Operation {
    Comment {
        pull_request: u32,
        body: String,
    },
    Update {
        pull_request: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<String>,
    },
    Merge {
        pull_request: u32,
        expected_head_sha: String,
        #[serde(default)]
        method: MergeMethod,
    },
}
impl Operation {
    pub fn pull_request(&self) -> u32 {
        match self {
            Self::Comment { pull_request, .. }
            | Self::Update { pull_request, .. }
            | Self::Merge { pull_request, .. } => *pull_request,
        }
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.pull_request() == 0 {
            return Err("Supply a positive PR number");
        }
        match self {
            Self::Comment { body, .. } if body.trim().is_empty() || body.len() > 16000 => {
                Err("Comment must contain 1–16000 UTF-8 bytes")
            }
            Self::Update { title, body, .. }
                if (title.is_none() && body.is_none())
                    || title
                        .as_ref()
                        .is_some_and(|s| s.trim().is_empty() || s.len() > 200)
                    || body.as_ref().is_some_and(|s| s.len() > 16000) =>
            {
                Err("Supply title (1–200 UTF-8 bytes) or body (0–16000 UTF-8 bytes)")
            }
            Self::Merge {
                expected_head_sha, ..
            } if expected_head_sha.len() != 40
                || !expected_head_sha.bytes().all(|b| b.is_ascii_hexdigit()) =>
            {
                Err("Merge requires the exact 40-character PR head SHA")
            }
            _ => Ok(()),
        }
    }
}

pub enum Command {
    Read(Query),
    Write(Operation),
}
impl Command {
    pub fn parse(text: &str) -> Result<Self, &'static str> {
        let (number, rest) = text
            .trim()
            .split_once(char::is_whitespace)
            .unwrap_or((text.trim(), ""));
        let (action, tail) = rest
            .trim_start()
            .split_once(char::is_whitespace)
            .unwrap_or((rest.trim(), ""));
        let tail = tail.trim();
        if !matches!(action, "comment" | "title" | "body" | "merge") {
            return Query::parse(text).map(Self::Read);
        }
        let pull_request = number.parse().map_err(|_| USAGE)?;
        let operation = match action {
            "comment" => Operation::Comment {
                pull_request,
                body: tail.into(),
            },
            "title" => Operation::Update {
                pull_request,
                title: Some(tail.into()),
                body: None,
            },
            "body" => Operation::Update {
                pull_request,
                title: None,
                body: Some(tail.into()),
            },
            _ => {
                let parts = tail.split_whitespace().collect::<Vec<_>>();
                if !(1..=2).contains(&parts.len()) {
                    return Err(USAGE);
                }
                Operation::Merge {
                    pull_request,
                    expected_head_sha: parts[0].into(),
                    method: match parts.get(1).copied().unwrap_or("squash") {
                        "squash" => MergeMethod::Squash,
                        "merge" => MergeMethod::Merge,
                        "rebase" => MergeMethod::Rebase,
                        _ => return Err(USAGE),
                    },
                }
            }
        };
        operation.validate()?;
        Ok(Self::Write(operation))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Section {
    #[default]
    Overview,
    Files,
    Comments,
    Reviews,
    ReviewComments,
    Checks,
    Statuses,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    pub pull_request: u32,
    #[serde(default)]
    pub section: Section,
    #[serde(default = "first_page")]
    pub page: u16,
}
fn first_page() -> u16 {
    1
}
impl Query {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.pull_request == 0
            || !(1..=1000).contains(&self.page)
            || (self.section == Section::Overview && self.page != 1)
        {
            return Err("Supply a positive PR number and page 1–1000; overview uses page 1");
        }
        Ok(())
    }
    pub fn parse(text: &str) -> Result<Self, &'static str> {
        let words = text.split_whitespace().collect::<Vec<_>>();
        if !(1..=3).contains(&words.len()) {
            return Err(USAGE);
        }
        let section = match words.get(1).copied().unwrap_or("overview") {
            "overview" => Section::Overview,
            "files" => Section::Files,
            "comments" => Section::Comments,
            "reviews" => Section::Reviews,
            "review-comments" => Section::ReviewComments,
            "checks" => Section::Checks,
            "statuses" => Section::Statuses,
            _ => return Err(USAGE),
        };
        let query = Self {
            pull_request: words[0].parse().map_err(|_| USAGE)?,
            section,
            page: words
                .get(2)
                .map_or(Ok(1), |word| word.parse().map_err(|_| USAGE))?,
        };
        query.validate()?;
        Ok(query)
    }
}

#[cfg(test)]
mod tests;
