//! Schema evolution as small bidirectional lenses, after Ink & Switch's
//! Cambria (<https://www.inkandswitch.com/cambria/>).
//!
//! A persisted document and its patch log carry the [`Schema`] they were
//! written with. Reading translates on read: whole documents with
//! [`Lens::document`] and individual patches with [`Lens::edit`], so a log
//! written by one version replays in another without first rewriting it.
//! Every lens has an inverse ([`Lens::reverse`]); a schema stores its own
//! lenses as data, so an older reader can walk a newer writer's steps
//! backwards. That is the rollback story: an older build with this module
//! reads newer state through the lenses recorded in it.
//!
//! The vocabulary is deliberately the small part of Cambria Control needs:
//! rename, add/remove with a default, wrap/head, and `in`/`map` to scope a
//! lens to a member or to every array element. History is linear (one step
//! per version), not Cambria's general lens graph. Hoist/plunge and value
//! conversion are left out until a migration needs them; moves between
//! documents (such as finished runs leaving the state) are outside any lens,
//! as Cambria also notes, and stay explicit code.
use crate::snapshot::{Change, Edit};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lens {
    Rename { from: String, to: String },
    /// Adds a member, filled with `default` where missing.
    Add { name: String, default: Value },
    /// Removes a member; reversing it restores `default`.
    Remove { name: String, default: Value },
    /// A scalar member becomes a one-element array (null becomes empty).
    Wrap { name: String },
    /// An array member becomes its first element (empty becomes null).
    Head { name: String },
    /// Applies lenses inside one member.
    In { name: String, lens: Vec<Lens> },
    /// Applies lenses to every element of an array.
    Map { lens: Vec<Lens> },
    /// Part of `name` moved out of the document (for example into archive
    /// files). Reading forward needs nothing; there is no way back, so an
    /// older reader refuses rather than silently missing that part.
    External { name: String },
}

/// The schema a document was written with: its version and, for each step
/// from version `i` to `i + 1`, the lenses of that step.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Schema {
    pub version: u32,
    #[serde(default)]
    pub lenses: Vec<Vec<Lens>>,
}

impl Lens {
    pub fn reverse(&self) -> Lens {
        match self {
            Self::Rename { from, to } => Self::Rename { from: to.clone(), to: from.clone() },
            Self::Add { name, default } => Self::Remove { name: name.clone(), default: default.clone() },
            Self::Remove { name, default } => Self::Add { name: name.clone(), default: default.clone() },
            Self::Wrap { name } => Self::Head { name: name.clone() },
            Self::Head { name } => Self::Wrap { name: name.clone() },
            Self::In { name, lens } => Self::In { name: name.clone(), lens: reverse(lens) },
            Self::Map { lens } => Self::Map { lens: reverse(lens) },
            Self::External { name } => Self::External { name: name.clone() },
        }
    }

    /// Translates a whole document (or the value of a patch that replaces one).
    pub fn document(&self, value: &mut Value) {
        if let Self::Map { lens } = self {
            for item in value.as_array_mut().into_iter().flatten() {
                document(lens, item);
            }
            return;
        }
        let Value::Object(map) = value else { return };
        match self {
            Self::Rename { from, to } => {
                if let Some(member) = map.remove(from) {
                    map.insert(to.clone(), member);
                }
            }
            Self::Add { name, default } => {
                map.entry(name.clone()).or_insert_with(|| default.clone());
            }
            Self::Remove { name, .. } => {
                map.remove(name);
            }
            Self::Wrap { name } => {
                if let Some(member) = map.get_mut(name) {
                    *member = wrap(member.take());
                }
            }
            Self::Head { name } => {
                if let Some(member) = map.get_mut(name) {
                    *member = head(member.take());
                }
            }
            Self::In { name, lens } => {
                if let Some(member) = map.get_mut(name) {
                    document(lens, member);
                }
            }
            Self::Map { .. } => unreachable!("handled above"),
            Self::External { .. } => {}
        }
    }

    /// Translates one patch edit; `None` when the edit has no counterpart
    /// (it changes a member the other schema does not have).
    pub fn edit(&self, Edit { mut path, change }: Edit) -> Option<Edit> {
        let Some(first) = path.first().cloned() else {
            let change = match change {
                Change::Set(mut value) => {
                    self.document(&mut value);
                    Change::Set(value)
                }
                Change::Splice { at, delete, mut insert } => {
                    if let Self::Map { lens } = self {
                        insert.iter_mut().for_each(|item| document(lens, item));
                    }
                    Change::Splice { at, delete, insert }
                }
                Change::Remove => Change::Remove,
            };
            return Some(Edit { path, change });
        };
        let named = |name: &str| first.as_str() == Some(name);
        match self {
            Self::Rename { from, to } if named(from) => path[0] = Value::String(to.clone()),
            Self::Add { name, default } if named(name) && path.len() == 1 && change == Change::Remove => {
                return Some(Edit { path, change: Change::Set(default.clone()) });
            }
            Self::Remove { name, .. } if named(name) => return None,
            Self::Wrap { name } if named(name) => {
                if path.len() == 1 {
                    let change = match change {
                        Change::Set(value) => Change::Set(wrap(value)),
                        other => other,
                    };
                    return Some(Edit { path, change });
                }
                path.insert(1, Value::from(0));
            }
            Self::Head { name } if named(name) => {
                if path.len() == 1 {
                    // Cambria's scalar/array ambiguity: only the head survives.
                    let change = match change {
                        Change::Set(value) => Change::Set(head(value)),
                        Change::Splice { at: 0, insert, .. } if !insert.is_empty() => {
                            Change::Set(insert.into_iter().next().expect("not empty"))
                        }
                        Change::Splice { .. } => return None,
                        Change::Remove => Change::Remove,
                    };
                    return Some(Edit { path, change });
                }
                if path[1].as_u64() != Some(0) {
                    return None;
                }
                path.remove(1);
            }
            Self::In { name, lens } if named(name) => {
                let inner = edit(lens, Edit { path: path.split_off(1), change })?;
                path.extend(inner.path);
                return Some(Edit { path, change: inner.change });
            }
            Self::Map { lens } if first.is_u64() => {
                let inner = edit(lens, Edit { path: path.split_off(1), change })?;
                path.extend(inner.path);
                return Some(Edit { path, change: inner.change });
            }
            _ => {}
        }
        Some(Edit { path, change })
    }
}
fn wrap(value: Value) -> Value {
    match value {
        Value::Null => Value::Array(Vec::new()),
        value => Value::Array(vec![value]),
    }
}
fn head(value: Value) -> Value {
    match value {
        Value::Array(items) => items.into_iter().next().unwrap_or(Value::Null),
        value => value,
    }
}

pub fn reverse(lenses: &[Lens]) -> Vec<Lens> {
    lenses.iter().rev().map(Lens::reverse).collect()
}
pub fn document(lenses: &[Lens], value: &mut Value) {
    lenses.iter().for_each(|lens| lens.document(value));
}
pub fn edit(lenses: &[Lens], edit: Edit) -> Option<Edit> {
    lenses.iter().try_fold(edit, |edit, lens| lens.edit(edit))
}

/// The lenses that take documents written with `from` to `to`: forward with
/// the reader's own steps, or backward through the steps the newer writer
/// recorded.
pub fn route(from: &Schema, to: &Schema) -> io::Result<Vec<Lens>> {
    let missing = || io::Error::new(io::ErrorKind::InvalidData, format!("No lenses from state schema {} to {}", from.version, to.version));
    let (low, high) = (from.version.min(to.version) as usize, from.version.max(to.version) as usize);
    let steps = if from.version <= to.version { &to.lenses } else { &from.lenses };
    let lenses: Vec<Lens> = steps.get(low..high).ok_or_else(missing)?.concat();
    if from.version > to.version
        && let Some(Lens::External { name }) = lenses.iter().find(|lens| matches!(lens, Lens::External { .. }))
    {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("State schema {} keeps part of {name} outside the state; schema {} cannot read it", from.version, to.version)));
    }
    Ok(if from.version <= to.version { lenses } else { reverse(&lenses) })
}

#[cfg(test)]
#[path = "lens/tests.rs"]
mod tests;
