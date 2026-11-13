
use super::*;

fn store(data: &[i64], pos: usize) -> Store<'_, [i64]> {
    Store::new(data, pos).unwrap()
}
fn sum3(s: Store<'_, [i64]>) -> i64 {
    s.window(1).iter().sum()
}

#[test]
fn comonad_laws_hold_for_timelines() {
    let data = [3, 1, 4, 1, 5, 9, 2, 6];
    for pos in 0..data.len() {
        let w = store(&data, pos);
        // extend extract = id
        assert_eq!(w.extend(|s| *s.extract()), data);
        // extract . extend f = f
        assert_eq!(w.extend(sum3)[w.pos()], sum3(w));
        // extend f . extend g = extend (f . extend g)
        let g = w.extend(sum3);
        let left = store(&g, pos).extend(|s| s.window(1).iter().sum::<i64>());
        let right = w.extend(|s| {
            store(&s.extend(sum3), s.pos())
                .window(1)
                .iter()
                .sum::<i64>()
        });
        assert_eq!(left, right);
    }
    assert!(Store::new(&data[..], data.len()).is_none());
}

#[test]
fn moves_and_views_are_bounded_slices() {
    let data = [10, 20, 30, 40];
    let w = store(&data, 1);
    assert_eq!(
        (w.window(5), w.since(), w.before()),
        (&data[..], &data[1..], &data[..1])
    );
    assert_eq!(w.left().map(|s| *s.extract()), Some(10));
    assert_eq!(
        w.right().and_then(|s| s.right()).map(|s| *s.extract()),
        Some(40)
    );
    assert!(store(&data, 0).left().is_none() && store(&data, 3).right().is_none());
    assert_eq!(w.peek(3), Some(&40));
}
