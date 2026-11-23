
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
