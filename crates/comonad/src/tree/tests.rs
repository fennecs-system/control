
use super::*;

/// root ─┬─ a ─┬─ a1
///       │     └─ a2 ── a2x
///       └─ b
fn sample() -> (Tree<&'static str>, [NodeId; 6]) {
    let mut t = Tree::default();
    let root = t.push(None, "root");
    let a = t.push(Some(root), "a");
    let a1 = t.push(Some(a), "a1");
    let b = t.push(Some(root), "b");
    let a2 = t.push(Some(a), "a2");
    let a2x = t.push(Some(a2), "a2x");
    (t, [root, a, a1, b, a2, a2x])
}
fn names<'a>(nodes: impl Iterator<Item = Focus<'a, &'static str>>) -> Vec<&'static str> {
    nodes.map(|n| *n.extract()).collect()
}

#[test]
fn zipper_moves_are_local_and_reversible() {
    let (t, [root, a, a1, b, a2, a2x]) = sample();
    let f = t.focus(a2x).unwrap();
    assert_eq!(names(f.ancestors()), ["a2x", "a2", "a", "root"]);
    assert_eq!((f.root().id(), f.depth()), (root, 3));
    let a = t.focus(a).unwrap();
    assert_eq!(names(a.children()), ["a1", "a2"]);
    assert_eq!(a.down().unwrap().next().unwrap().id(), a2);
    assert_eq!(a.last_child().unwrap().prev().unwrap().id(), a1);
    assert_eq!(a.next().unwrap().id(), b);
    assert_eq!(a.up().unwrap().id(), root);
    assert!(t.focus(root).unwrap().up().is_none());
    assert!(t.focus(NodeId(99)).is_none());
}

#[test]
fn subtrees_walk_preorder_and_stay_inside() {
    let (t, [root, a, a1, b, a2, a2x]) = sample();
    assert_eq!(
        names(t.focus(root).unwrap().subtree()),
        ["root", "a", "a1", "a2", "a2x", "b"]
    );
    assert_eq!(
        names(t.focus(a).unwrap().subtree()),
        ["a", "a1", "a2", "a2x"]
    );
    assert_eq!(names(t.focus(a2x).unwrap().subtree()), ["a2x"]);
    let a = t.focus(a).unwrap();
    assert!(a.contains(a2x) && a.contains(a1) && !a.contains(b) && !a.contains(root));
    let _ = a2;
}

#[test]
fn folds_see_children_before_parents() {
    let (t, [root, a, ..]) = sample();
    let sizes = t.fold(|_, kids| 1 + kids.copied().sum::<usize>());
    assert_eq!((sizes[root.index()], sizes[a.index()]), (6, 4));
}
