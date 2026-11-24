use super::*;

#[test]
fn levels_follow_the_turn() {
    let mut adaptive = Adaptive::new(Thinking::Medium, false);
    assert_eq!(adaptive.level(false), Thinking::High, "first step");
    adaptive.after(Cue::Routine);
    assert_eq!(adaptive.level(false), Thinking::Low);
    assert_eq!(adaptive.level(true), Thinking::High, "after a compaction");
    adaptive.after(Cue::Trouble);
    assert_eq!(adaptive.level(false), Thinking::High);
    adaptive.after(Cue::Cut);
    assert_eq!(adaptive.level(false), Thinking::Minimal);
    assert_eq!(adaptive.switch("Done reading. /no_think"), Some(Thinking::Off));
    assert_eq!(adaptive.level(false), Thinking::Off, "once");
    assert_eq!(adaptive.level(false), Thinking::Minimal);
    let mut resumed = Adaptive::new(Thinking::High, true);
    assert_eq!(resumed.level(false), Thinking::Minimal);
    resumed.after(Cue::Trouble);
    assert_eq!(resumed.level(true), Thinking::Minimal, "until it acts");
    resumed.acted();
    assert_eq!(resumed.level(false), Thinking::High);
    let mut off = Adaptive::new(Thinking::Off, false);
    assert_eq!((off.switch("/think"), off.level(true)), (None, Thinking::Off));
    assert_eq!(switch("try /think, then /no_think."), Some(Thinking::Off));
    assert_eq!(switch("path /thinker or a/think"), None);
    assert!(asks_more(" /think\n") && !asks_more("/think about it"));
}
