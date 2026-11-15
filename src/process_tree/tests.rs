use super::*;

fn tree(edges: &[(usize, usize)], n: usize) -> Vec<Vec<usize>> {
    let mut children = vec![Vec::new(); n];
    for &(parent, child) in edges {
        children[parent].push(child);
    }
    children
}
/// Tidy-drawing properties: parents centred, sibling order kept, and nodes on
/// a level at least one unit apart in left-to-right preorder.
fn assert_tidy(children: &[Vec<usize>]) {
    let placed = layout(children);
    let mut levels: Vec<Vec<f64>> = Vec::new();
    let mut stack = vec![0];
    let mut order = Vec::new();
    while let Some(v) = stack.pop() {
        order.push(v);
        stack.extend(children[v].iter().rev());
    }
    for &v in &order {
        let (x, depth) = placed[v];
        if levels.len() <= depth {
            levels.resize(depth + 1, Vec::new());
        }
        levels[depth].push(x);
        if let (Some(&first), Some(&last)) = (children[v].first(), children[v].last()) {
            assert!((x - (placed[first].0 + placed[last].0) / 2.0).abs() < 1e-9, "node {v} centred over its children");
        }
    }
    for level in levels {
        for pair in level.windows(2) {
            assert!(pair[1] - pair[0] >= 1.0 - 1e-9, "separation on a level: {pair:?}");
        }
    }
    assert!(placed.iter().any(|(x, _)| x.abs() < 1e-9), "normalised to start at zero");
}

#[test]
fn layout_centres_parents_and_spaces_levels() {
    assert_eq!(layout(&[Vec::new()]), vec![(0.0, 0)]);
    let fan = tree(&[(0, 1), (0, 2), (0, 3)], 4);
    assert_eq!(layout(&fan), vec![(1.0, 0), (0.0, 1), (1.0, 1), (2.0, 1)]);
    assert_tidy(&fan);
}

#[test]
fn small_middle_subtrees_are_spread_evenly_between_wide_neighbours() {
    // Walker's improvement over Reingold–Tilford: the leaf between two wide
    // subtrees is centred in the gap rather than packed to the left.
    let children = tree(&[(0, 1), (0, 2), (0, 3), (1, 4), (1, 5), (1, 6), (3, 7), (3, 8), (3, 9)], 10);
    let placed = layout(&children);
    assert_tidy(&children);
    let (a, b, c) = (placed[1].0, placed[2].0, placed[3].0);
    assert!((b - a - (c - b)).abs() < 1e-9, "middle leaf centred: {a} {b} {c}");
}

#[test]
fn identical_subtrees_are_drawn_identically_and_deep_contours_do_not_collide() {
    // Two copies of the same shape under one root, then a deep left subtree
    // whose lowest level must clear a shallow right neighbour's descendants.
    let children = tree(&[(0, 1), (0, 2), (1, 3), (1, 4), (2, 5), (2, 6), (3, 7), (5, 8)], 9);
    let placed = layout(&children);
    assert_tidy(&children);
    let shape = |root: usize, kids: [usize; 3]| kids.map(|k| placed[k].0 - placed[root].0);
    assert_eq!(shape(1, [3, 4, 7]), shape(2, [5, 6, 8]));

    let deep = tree(&[(0, 1), (0, 2), (0, 3), (1, 4), (4, 5), (5, 6), (5, 7), (5, 8), (3, 9), (9, 10), (10, 11), (10, 12)], 13);
    assert_tidy(&deep);
}

#[test]
fn many_generated_trees_stay_tidy() {
    let mut seed = 0x2545_f491_u64;
    for size in 2..120 {
        let mut children = vec![Vec::new(); size];
        for child in 1..size {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let parent = (seed >> 33) as usize % child;
            children[parent].push(child);
        }
        assert_tidy(&children);
    }
}

#[test]
fn the_tree_shows_bots_services_and_processes_of_active_task_trees() {
    let mut state = State::default();
    assert!(render(&state).is_none());
    let lead = crate::chat::create(&mut state);
    crate::chat::send(&mut state, &lead, "Run a <civilisation>", "test").unwrap();
    let worker = crate::chat::create(&mut state);
    state.chats.iter_mut().find(|chat| chat.id == worker).unwrap().parent_chat = Some(lead.clone());
    let html = render(&state).unwrap();
    assert!(html.contains("<svg") && html.contains("pt-steering") && html.contains("pt-agent"));
    assert!(html.contains("Run a &lt;civilisation&gt;") && !html.contains("<civilisation>"));
    assert!(html.contains(&format!("href='/chat/{worker}'")));
    state.runtime.runs.iter_mut().for_each(|run| run.status = "completed".into());
    assert!(render(&state).is_none(), "trees without bots, services or active turns are omitted");
}
#[test]
fn sibling_titles_drop_their_shared_template_words() {
    assert_eq!(distinct_titles(&["Own civilisation bot CivFarmer: food", "Own civilisation bot CivMiner: tools"]), ["CivFarmer: food", "CivMiner: tools"]);
    assert_eq!(distinct_titles(&["Own farm", "Own mine"]), ["Own farm", "Own mine"], "a single shared word is kept");
    assert_eq!(distinct_titles(&["Same title here", "Same title here"]), ["Same title here", "Same title here"]);
    assert_eq!(distinct_titles(&["Alone"]), ["Alone"]);
    assert_eq!(distinct_titles(&["Own civilisation bot A: x", "Own civilisation bot B: y", "Own server infrastructure"]), ["A: x", "B: y", "Own server infrastructure"], "a template shared by some siblings is still dropped");
}
