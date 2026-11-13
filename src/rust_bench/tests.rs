
use super::*;
#[test]
fn validates_before_execution_and_separates_timing_from_diagnostics() {
    for args in [
        json!({"code":""}),
        json!({"code":"1","iterations":0}),
        json!({"code":"1","samples":32}),
        json!({"code":"1","mount":"/"}),
    ] {
        assert!(execute(&args, |_| panic!("Invalid inputs must not execute")).is_err());
    }
    let result=execute(&json!({"code":"std::hint::black_box(7)+1","iterations":10,"samples":3}),|source|{
            assert!(source.contains("cfg(debug_assertions)"));
            assert!(source.contains("Instant::now()"));
            Ok(json!({"success":true,"output":"compiler warning\nCONTROL_BENCH_V1 [20,40,30]\n","image":"pinned-fixture"}))
        }).unwrap();
    assert_eq!(result["timings"]["median_ns"], 3.0);
    assert_eq!(result["image"], "pinned-fixture");
    assert!(
        execute(&json!({"code":"1","samples":3}), |_| Ok(
            json!({"success":true,"output":"CONTROL_BENCH_V1 [1]"})
        ))
        .is_err()
    );
    let failed = execute(&json!({"code":"bad rust"}), |_| {
        Ok(json!({"success":false,"status":"compile 1"}))
    })
    .unwrap();
    assert!(failed["timings"].is_null());
}
