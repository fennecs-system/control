use super::super::{apply, diff, diff_trees, edits};
use super::*;
use serde::Serialize;

#[derive(Clone, Serialize)]
struct Item {
    name: String,
    n: u64,
    tags: Vec<String>,
}
#[derive(Clone, Default, Serialize)]
struct Inner {
    note: String,
    items: Vec<Part<Item>>,
    plain: Vec<u64>,
}
/// Shaped like a chat: no parts inside, a message list that grows past
/// `SEGMENT`.
#[derive(Clone, Serialize)]
struct Chat {
    id: String,
    revision: u64,
    messages: Vec<String>,
}
#[derive(Clone, Default, Serialize)]
struct Doc {
    items: Part<Vec<Part<Item>>>,
    chats: Part<Vec<Part<Chat>>>,
    inner: Part<Inner>,
    words: Part<Vec<String>>,
    plain: Vec<String>,
    #[serde(serialize_with = "split_each_save")]
    each: Inner,
    #[serde(skip_serializing_if = "Option::is_none")]
    maybe: Option<Part<String>>,
    title: Part<String>,
}

/// A small deterministic generator: tests need no dependency.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self, below: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) as usize) % below.max(1)
    }
}
fn item(rng: &mut Lcg) -> Item {
    let long = rng.next(4) == 0;
    Item {
        name: if long {
            "long ".repeat(80)
        } else {
            format!("item {}", rng.next(1000))
        },
        n: rng.next(100) as u64,
        tags: (0..rng.next(4)).map(|i| format!("t{i}")).collect(),
    }
}
fn mutate(doc: &mut Doc, rng: &mut Lcg) {
    let pick = |rng: &mut Lcg, len: usize| rng.next(len);
    match rng.next(17) {
        12 => doc.chats.push(
            Chat {
                id: format!("c-{}", rng.next(1000)),
                revision: 0,
                messages: Vec::new(),
            }
            .into(),
        ),
        13 if !doc.chats.is_empty() => {
            let at = pick(rng, doc.chats.len());
            let chat = &mut doc.chats[at];
            chat.messages.push("message ".repeat(rng.next(40)));
            chat.revision += 1;
        }
        14 if !doc.chats.iter().all(|chat| chat.messages.is_empty()) => {
            let chat = doc.chats.iter_mut().find(|chat| !chat.messages.is_empty()).unwrap();
            let at = pick(rng, chat.messages.len());
            chat.messages[at] = format!("edited {}", rng.next(9));
        }
        15 if !doc.chats.is_empty() => {
            let at = pick(rng, doc.chats.len());
            doc.chats.remove(at);
        }
        // Found through a mutable borrow but only read: keeps its encoding.
        16 => drop(
            doc.chats
                .iter_mut()
                .find(|chat| chat.revision > 3)
                .map(|chat| chat.id.len()),
        ),
        0 | 1 => doc.items.push(item(rng).into()),
        2 if !doc.items.is_empty() => {
            let at = pick(rng, doc.items.len());
            doc.items.remove(at);
        }
        3 if !doc.items.is_empty() => {
            let at = pick(rng, doc.items.len());
            doc.items[at].n += 1;
        }
        4 => doc.inner.items.push(item(rng).into()),
        5 => doc.inner.note.push_str(&"x".repeat(rng.next(300))),
        6 => doc.words.push(format!("word {}", rng.next(50))),
        7 => doc.plain.push("plain".into()),
        8 => doc.each.items.push(item(rng).into()),
        9 if !doc.each.items.is_empty() => {
            let at = pick(rng, doc.each.items.len());
            doc.each.items[at].tags.push("changed".into());
        }
        10 => {
            doc.maybe = match doc.maybe.take() {
                Some(_) => None,
                None => Some(format!("maybe {}", rng.next(9)).into()),
            }
        }
        _ => *doc.title = format!("title {}", rng.next(1000)),
    }
}

#[test]
fn trees_diff_exactly_as_their_joined_encodings() {
    let mut rng = Lcg(7);
    let mut doc = Doc::default();
    let mut tree = split(&doc).unwrap();
    let mut flat = encode(&doc).unwrap();
    for step in 0..2_000 {
        let edits_now = 1 + rng.next(3);
        (0..edits_now).for_each(|_| mutate(&mut doc, &mut rng));
        let next = split(&doc).unwrap();
        let fresh = encode(&doc).unwrap();
        assert_eq!(
            next.bytes(),
            fresh.bytes(),
            "step {step}: the tree joins to the encoding"
        );
        assert_eq!(next.len(), fresh.bytes().len());
        let expected = diff(&flat, &fresh);
        assert_eq!(diff_trees(&tree, &next), expected, "step {step}: tree to tree");
        assert_eq!(
            diff_trees(&Tree::leaf(flat.clone()), &next),
            expected,
            "step {step}: adopted base"
        );
        assert_eq!(
            diff_trees(&tree, &Tree::leaf(fresh.clone())),
            expected,
            "step {step}: to a leaf"
        );
        if let Some(patch) = &expected {
            let mut document: serde_json::Value = serde_json::from_slice(flat.bytes()).unwrap();
            apply(&mut document, edits(patch).unwrap()).unwrap();
            assert_eq!(
                document,
                serde_json::from_slice::<serde_json::Value>(fresh.bytes()).unwrap()
            );
        }
        (tree, flat) = (next, fresh);
    }
}

#[test]
fn unchanged_parts_are_shared_and_any_mutable_borrow_forgets_the_tree() {
    let mut rng = Lcg(3);
    let mut doc = Doc::default();
    (0..40).for_each(|_| doc.items.push(item(&mut rng).into()));
    let before = split(&doc).unwrap();
    let saved = doc.items.saved().unwrap().clone();
    // Reading keeps the tree.
    assert!(doc.items.iter().all(|item| item.n < 1_000) && doc.items.saved().is_some());
    *doc.title = "renamed".into();
    let after = split(&doc).unwrap();
    assert!(
        Arc::ptr_eq(&saved.0, &doc.items.saved().unwrap().0),
        "untouched parts keep their tree"
    );
    assert_eq!(
        diff_trees(&before, &after),
        diff(
            &encode(&Doc {
                title: Part::from(String::new()),
                ..doc.clone()
            })
            .unwrap(),
            &encode(&doc).unwrap()
        )
    );
    // Finding an element through `iter_mut` forgets the list, not the
    // elements it only read.
    assert!(doc.items.iter_mut().position(|item| item.n == 1_000).is_none());
    assert!(doc.items.saved().is_none() && doc.items.iter().all(|item| item.saved().is_some()));
    split(&doc).unwrap();
    // An element borrowed mutably is re-encoded; its siblings are not.
    let sibling = doc.items[1].saved().unwrap().clone();
    doc.items[0].n += 1;
    assert!(doc.items.saved().is_none() && doc.items[0].saved().is_none());
    split(&doc).unwrap();
    assert!(Arc::ptr_eq(&sibling.0, &doc.items[1].saved().unwrap().0));
}

#[test]
fn values_without_parts_stay_one_encoding() {
    let item = Item {
        name: "x".repeat(300),
        n: 1,
        tags: Vec::new(),
    };
    assert!(matches!(*split(&item).unwrap().0, Shape::Leaf(_)));
    assert!(matches!(*split(&vec![1, 2, 3]).unwrap().0, Shape::Leaf(_)));
    let empty: Vec<Part<Item>> = Vec::new();
    assert!(matches!(*split(&empty).unwrap().0, Shape::Leaf(_)));
    // Serialization outside a split is the value's own.
    let part = Part::from(item.clone());
    assert_eq!(serde_json::to_vec(&part).unwrap(), serde_json::to_vec(&item).unwrap());
}

/// An object without parts is encoded member by member and joined: the
/// same bytes and ranges as encoding it whole, small or past `SEGMENT`.
#[test]
fn joined_members_encode_as_the_whole_value() {
    let leaf = |tree: Tree| match Arc::try_unwrap(tree.0).unwrap() {
        Shape::Leaf(encoded) => encoded,
        _ => panic!("a leaf"),
    };
    let mut rng = Lcg(7);
    for _ in 0..200 {
        let item = item(&mut rng);
        assert_eq!(leaf(split(&item).unwrap()), encode(&item).unwrap());
        let map: std::collections::BTreeMap<String, Item> = (0..rng.next(5))
            .map(|i| {
                (format!("k\"{i}"), {
                    let mut next = item.clone();
                    next.n = rng.next(9) as u64;
                    next.tags.push("a\\b".repeat(rng.next(90)));
                    next
                })
            })
            .collect();
        assert_eq!(leaf(split(&map).unwrap()), encode(&map).unwrap());
    }
}
