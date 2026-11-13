//! Typed GitHub inspection requests shared with the host broker. No credentials.
use serde::{Deserialize, Serialize};

pub const USAGE: &str =
    "/github PR [overview|files|comments|reviews|review-comments|checks|statuses] [PAGE]";

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
