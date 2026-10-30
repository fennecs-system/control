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
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn zero_bot_permissions_still_inherit_everyone() {
        let roles = vec![
            json!({"id":"1","permissions":(VIEW|HISTORY).to_string()}),
            json!({"id":"2","permissions":"0"}),
        ];
        let member = json!({"roles":["2"]});
        let channel = json!({"permission_overwrites":[]});
        assert_eq!(
            effective("1", "9", &member, &roles, &channel).unwrap() & (VIEW | HISTORY),
            VIEW | HISTORY
        );
    }
    #[test]
    fn member_deny_overrides_role_allow() {
        let roles = vec![
            json!({"id":"1","permissions":(VIEW|HISTORY).to_string()}),
            json!({"id":"2","permissions":"0"}),
        ];
        let member = json!({"roles":["2"]});
        let channel = json!({"permission_overwrites":[{"id":"2","type":0,"allow":VIEW.to_string(),"deny":"0"},{"id":"9","type":1,"allow":"0","deny":VIEW.to_string()}]});
        assert_eq!(
            effective("1", "9", &member, &roles, &channel).unwrap() & VIEW,
            0
        );
    }
    #[test]
    fn admin_bypasses_channel_denies() {
        let roles = vec![json!({"id":"1","permissions":ADMIN.to_string()})];
        assert_eq!(effective("1","9",&json!({"roles":[]}),&roles,&json!({"permission_overwrites":[{"id":"9","type":1,"allow":"0","deny":VIEW.to_string()}]})).unwrap(),u128::MAX);
    }
}
