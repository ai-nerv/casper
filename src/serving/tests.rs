use super::*;
use crate::framing::{self, MOST, Wire};
use crate::scratch::Scratch;
use std::time::Instant;

const KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn key() -> Key {
    Key::read(&mut format!("{KEY}\n").as_bytes()).expect("key")
}

/// What the test server answers: `slow` sleeps, `huge` is over the cap, anything else echoes.
fn answering(call: &Call) -> Reply {
    match call.call.as_str() {
        "slow" => {
            std::thread::sleep(Duration::from_secs(1));
            Reply::done()
        }
        "huge" => Reply::of(serde_json::json!("x".repeat(MOST + 1))),
        other => Reply::of(serde_json::json!({ "echoed": other })),
    }
}

/// A server on a scratch socket, and the path to dial.
fn serving() -> (Scratch, PathBuf) {
    let dir = Scratch::new("serving", "socket");
    let path = dir.join("s.sock");
    let listener = UnixListener::bind(&path).expect("bind");
    std::thread::spawn(move || accept(&listener, key(), answering));
    (dir, path)
}

fn dial(path: &Path) -> UnixStream {
    let stream = UnixStream::connect(path).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("deadline");
    stream
}

fn ask(stream: &mut UnixStream, wire: Wire, call: serde_json::Value) -> Reply {
    let body = wire.encode(&call).expect("encode");
    framing::write_frame(stream, &body).expect("send");
    heard(stream)
}

fn heard(stream: &mut UnixStream) -> Reply {
    let got = framing::read_frame(stream).expect("a reply");
    framing::decode(&got).expect("decode")
}

fn keyed(call: &str) -> serde_json::Value {
    serde_json::json!({ "call": call, "args": [], "key": KEY })
}

#[test]
fn a_path_outside_the_runtime_is_not_one_to_bind() {
    assert!(!inside(Path::new("/tmp/casper.sock")));
    assert!(!inside(&runtime().join("../escape")));
    assert!(!inside(&runtime()));
    assert!(inside(&runtime().join("proj/id")));
}

#[test]
fn a_call_with_its_key_is_answered_in_its_own_encoding() {
    let (_dir, path) = serving();
    let mut stream = dial(&path);
    for wire in [Wire::Json, Wire::Cbor] {
        let reply = ask(&mut stream, wire, keyed("tools"));
        assert!(reply.ok, "{reply:?}");
        assert_eq!(reply.result[0]["echoed"], "tools");
    }
}

#[test]
fn a_call_without_the_key_or_with_another_is_refused_and_the_connection_stays() {
    let (_dir, path) = serving();
    let mut stream = dial(&path);
    let other = KEY.replace('a', "b");
    for call in [
        serde_json::json!({ "call": "run", "args": [] }),
        serde_json::json!({ "call": "run", "args": [], "key": other }),
    ] {
        let reply = ask(&mut stream, Wire::Json, call);
        assert!(!reply.ok);
        assert_eq!(reply.error.as_deref(), Some(key::REFUSED));
        assert_eq!(reply.fault, Some(crate::wire::Fault::Refused));
    }
    assert!(ask(&mut stream, Wire::Json, keyed("tools")).ok);
}

#[test]
fn an_oversized_call_is_answered_failed_and_then_closed() {
    let (_dir, path) = serving();
    let mut stream = dial(&path);
    let header = u32::try_from(MOST + 1).expect("fits").to_be_bytes();
    std::io::Write::write_all(&mut stream, &header).expect("header");
    let reply = heard(&mut stream);
    assert_eq!(reply.fault, Some(crate::wire::Fault::Failed), "{reply:?}");
    assert!(reply.error.unwrap_or_default().contains("16777216"));
    let after = framing::read_frame(&mut stream).expect_err("closed");
    assert_eq!(after.kind(), ErrorKind::UnexpectedEof);
}

#[test]
fn a_zero_length_frame_is_refused_and_the_next_call_answered() {
    let (_dir, path) = serving();
    let mut stream = dial(&path);
    std::io::Write::write_all(&mut stream, &[0, 0, 0, 0]).expect("empty frame");
    let reply = heard(&mut stream);
    assert!(!reply.ok && reply.error.unwrap_or_default().contains("zero"));
    assert!(ask(&mut stream, Wire::Json, keyed("tools")).ok);
}

#[test]
fn a_call_from_a_newer_family_is_refused_by_name_and_the_next_call_answered() {
    let (_dir, path) = serving();
    let mut stream = dial(&path);
    for wire in [Wire::Json, Wire::Cbor] {
        let mut newer = keyed("tools");
        newer["family"] = serde_json::json!(crate::wire::FAMILY + 1);
        let reply = ask(&mut stream, wire, newer);
        assert!(!reply.ok, "{reply:?}");
        assert!(reply.error.unwrap_or_default().contains("family 2"));
    }
    let mut older = keyed("tools");
    older["family"] = serde_json::json!(crate::wire::FAMILY);
    assert!(ask(&mut stream, Wire::Json, older).ok);
}

#[test]
fn a_reply_over_the_cap_is_replaced_by_a_failure_and_the_connection_stays_in_step() {
    let (_dir, path) = serving();
    let mut stream = dial(&path);
    let reply = ask(&mut stream, Wire::Json, keyed("huge"));
    assert_eq!(reply.fault, Some(crate::wire::Fault::Failed), "{reply:?}");
    assert!(reply.error.unwrap_or_default().contains("cap"));
    let next = ask(&mut stream, Wire::Json, keyed("tools"));
    assert_eq!(next.result[0]["echoed"], "tools", "{next:?}");
}

#[test]
fn a_slow_call_does_not_hold_up_another_connection() {
    let (_dir, path) = serving();
    let mut slow = dial(&path);
    let body = Wire::Json.encode(&keyed("slow")).expect("encode");
    framing::write_frame(&mut slow, &body).expect("send");
    std::thread::sleep(Duration::from_millis(50));
    let began = Instant::now();
    assert!(ask(&mut dial(&path), Wire::Json, keyed("tools")).ok);
    let took = began.elapsed();
    assert!(took < Duration::from_millis(500), "{took:?}");
    assert!(heard(&mut slow).ok);
}

#[test]
fn past_the_limit_a_connection_is_told_busy_and_a_freed_slot_is_reused() {
    let (_dir, path) = serving();
    let mut held: Vec<UnixStream> = (0..CONNECTIONS).map(|_| dial(&path)).collect();
    for stream in &mut held {
        assert!(ask(stream, Wire::Json, keyed("tools")).ok);
    }
    let mut turned = dial(&path);
    let reply = heard(&mut turned);
    assert_eq!(reply.fault, Some(crate::wire::Fault::Failed), "{reply:?}");
    assert!(reply.error.unwrap_or_default().starts_with("busy"));
    assert!(framing::read_frame(&mut turned).is_err(), "closed after");
    held.pop();
    let deadline = Instant::now() + Duration::from_secs(5);
    let body = Wire::Json.encode(&keyed("tools")).expect("encode");
    loop {
        let mut stream = dial(&path);
        let answered = framing::write_frame(&mut stream, &body)
            .and_then(|()| framing::read_frame(&mut stream))
            .ok()
            .and_then(|got| framing::decode::<Reply>(&got).ok());
        if answered.is_some_and(|reply| reply.ok) {
            break;
        }
        assert!(Instant::now() < deadline, "the slot never came back");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn an_accept_that_runs_out_of_descriptors_rests_and_carries_on() {
    let (served, mut client) = UnixStream::pair().expect("pair");
    let body = Wire::Json.encode(&keyed("tools")).expect("encode");
    framing::write_frame(&mut client, &body).expect("send");
    let mut queue: Vec<std::io::Result<UnixStream>> = vec![
        Err(std::io::Error::from_raw_os_error(libc::EBADF)),
        Ok(served),
        Err(std::io::Error::from_raw_os_error(libc::ENFILE)),
        Err(std::io::Error::from_raw_os_error(libc::EMFILE)),
    ];
    let ended = accepting(|| queue.pop().expect("queued"), key(), answering);
    assert_eq!(ended.expect_err("ends").raw_os_error(), Some(libc::EBADF));
    client
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("deadline");
    assert!(heard(&mut client).ok);
}
