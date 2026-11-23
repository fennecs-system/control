//! Evaluate permissions from metadata only. Never fetch guild message contents.
use serde_json::Value;
use std::collections::HashSet;
pub const ADMIN: u128 = 1 << 3;
pub const VIEW: u128 = 1 << 10;
pub const HISTORY: u128 = 1 << 16;
pub const MANAGE_ROLES: u128 = 1 << 28;
fn bits(value: &Value) -> Result<u128, &'static str> {
    value
        .as_str()
        .and_then(|s| s.parse().ok())
        .ok_or("Invalid permission bitfield")
}
pub fn effective(
    guild: &str,
    bot: &str,
    member: &Value,
    roles: &[Value],
    channel: &Value,
) -> Result<u128, &'static str> {
    let assigned = member["roles"]
        .as_array()
        .ok_or("Missing bot role membership")?;
    let mut ids = HashSet::from([guild]);
    for id in assigned {
        ids.insert(id.as_str().ok_or("Invalid bot role ID")?);
    }
    let mut base = 0;
    for id in &ids {
        let role = roles
            .iter()
            .find(|r| r["id"].as_str() == Some(*id))
            .ok_or("Assigned role metadata unavailable")?;
        base |= bits(&role["permissions"])?;
    }
    if base & ADMIN != 0 {
        return Ok(u128::MAX);
    }
    let overwrites = channel["permission_overwrites"]
        .as_array()
        .ok_or("Missing channel permission metadata")?;
    for row in overwrites
        .iter()
        .filter(|r| r["id"].as_str() == Some(guild) && r["type"].as_u64() == Some(0))
    {
        base = (base & !bits(&row["deny"])?) | bits(&row["allow"])?;
    }
    let (mut allow, mut deny) = (0, 0);
    for row in overwrites.iter().filter(|r| {
        r["type"].as_u64() == Some(0)
            && r["id"]
                .as_str()
                .is_some_and(|id| id != guild && ids.contains(id))
    }) {
        allow |= bits(&row["allow"])?;
        deny |= bits(&row["deny"])?;
    }
    base = (base & !deny) | allow;
    for row in overwrites
        .iter()
        .filter(|r| r["type"].as_u64() == Some(1) && r["id"].as_str() == Some(bot))
    {
        base = (base & !bits(&row["deny"])?) | bits(&row["allow"])?;
    }
    Ok(base)
}
#[cfg(test)]
mod tests;
