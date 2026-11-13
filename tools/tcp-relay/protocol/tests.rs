
use super::*;
#[test]
fn framing_is_bounded_and_half_close_is_distinct_from_transport_eof() {
    let mut data = Vec::new();
    write(&mut data, Kind::Data, b"hello").unwrap();
    write(&mut data, Kind::Fin, &[]).unwrap();
    write(&mut data, Kind::Ping, &[]).unwrap();
    let mut input = data.as_slice();
    let mut buffer = [0; LIMIT];
    assert_eq!(read(&mut input, &mut buffer).unwrap(), (Kind::Data, 5));
    assert_eq!(&buffer[..5], b"hello");
    assert_eq!(read(&mut input, &mut buffer).unwrap(), (Kind::Fin, 0));
    assert_eq!(read(&mut input, &mut buffer).unwrap(), (Kind::Ping, 0));
    assert!(read(&mut input, &mut buffer).is_err());
    assert!(write(&mut data, Kind::Data, &vec![0; LIMIT + 1]).is_err());
    assert!(read(&mut [1, 0, 1, 0, 0].as_slice(), &mut buffer).is_err());
    assert!(read(&mut [7, 0, 0, 0, 0].as_slice(), &mut buffer).is_err());
}
#[test]
fn credit_is_bounded_by_the_window_and_never_reused() {
    let credit = Credit::default();
    let alive = AtomicBool::new(true);
    for _ in 0..WINDOW {
        credit.grant().unwrap();
    }
    assert!(
        credit.grant().is_err(),
        "a peer cannot grant more than the window"
    );
    for _ in 0..WINDOW {
        credit.take(&alive).unwrap();
    }
    credit.grant().unwrap();
    credit.take(&alive).unwrap();
    alive.store(false, Ordering::Release);
    assert!(credit.take(&alive).is_err());
}
