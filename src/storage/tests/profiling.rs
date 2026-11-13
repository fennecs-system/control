use super::*;

#[test]
#[ignore = "release startup checkpoint timing fixture; use fresh scratch config/state directories, no models"]
fn profile_checkpoint_load() {
    use std::{hint::black_box, time::Instant};
    assert!(std::env::var_os("CONTROL_STATE_DIR").is_some());
    assert!(std::env::var_os("CONTROL_CONFIG_DIR").is_some());
    let directory = crate::config::state_directory().unwrap();
    fs::create_dir_all(&directory).unwrap();
    assert!(
        !directory.join("state.json").exists(),
        "use a fresh scratch directory"
    );
    save(&State::initial()).unwrap();
    black_box(load().unwrap());
    let bytes = fs::metadata(directory.join("state.json")).unwrap().len();
    let mut samples = Vec::new();
    for round in 0..51 {
        let start = Instant::now();
        black_box(load().unwrap());
        if round != 0 {
            samples.push(start.elapsed().as_nanos());
        }
    }
    samples.sort_unstable();
    println!(
        "{}",
        serde_json::json!({"case":"checkpoint_load", "state_bytes":bytes,
        "median_ns":(samples[24]+samples[25])/2, "min_ns":samples[0], "p95_ns":samples[46]})
    );
}
