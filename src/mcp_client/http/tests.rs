use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::AtomicUsize;

use rmcp::model::ServerJsonRpcMessage;
use serde_json::json;

use super::*;

/// A server answering every request with `body` as an SSE stream, then closing the connection.
/// Counts the requests it was sent.
fn serving(body: String) -> (Arc<str>, Arc<AtomicUsize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
    let url = format!("http://{}/mcp", listener.local_addr().expect("address"));
    let asked = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&asked);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            counted.fetch_add(1, Ordering::Relaxed);
            let mut reader = BufReader::new(stream);
            let mut length = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some((name, value)) = line.split_once(':')
                    && name.eq_ignore_ascii_case("content-length")
                {
                    length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut request = vec![0; length];
            let _ = reader.read_exact(&mut request);
            let mut stream = reader.into_inner();
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{body}"
            );
        }
    });
    (url.into(), asked)
}

fn client(cap: usize) -> (Bounded, Arc<AtomicBool>) {
    let over = Arc::new(AtomicBool::new(false));
    crate::web::crypto();
    let inner = reqwest::Client::builder()
        .no_proxy()
        .no_gzip()
        .build()
        .expect("client");
    (Bounded::new(inner, cap, Arc::clone(&over)), over)
}

fn ping() -> ClientJsonRpcMessage {
    serde_json::from_value(json!({"jsonrpc":"2.0","id":1,"method":"ping"})).expect("request")
}

/// One SSE event with an id, carrying `message`.
fn event(id: u32, message: &serde_json::Value) -> String {
    format!("id: {id}\ndata: {message}\n\n")
}

fn note(said: &str) -> serde_json::Value {
    json!({"jsonrpc":"2.0","method":"notifications/message","params":{"level":"info","data":said}})
}

#[tokio::test]
async fn a_posts_sse_reply_reaches_rmcp_whole_and_its_notification_first() {
    let answer = json!({"jsonrpc":"2.0","id":1,"result":{}});
    let body = [
        event(1, &note("halfway")),
        ": a comment between events\n".to_owned(),
        event(2, &note("nearly")),
        event(3, &answer),
    ]
    .concat();
    let (url, _) = serving(body.clone());
    let (client, over) = client(160);
    assert!(body.len() > 160, "the cap holds an event, not the stream");

    let reply = client
        .post_message(url, ping(), None, None, HashMap::new())
        .await
        .expect("reply");
    let StreamableHttpPostResponse::Sse(stream, _) = reply else {
        panic!("not a stream: {reply:?}");
    };
    let events: Vec<_> = stream.collect().await;
    let events: Vec<_> = events.into_iter().map(|one| one.expect("event")).collect();
    let ids: Vec<_> = events.iter().map(|one| one.id.as_deref()).collect();
    assert_eq!(ids, [Some("1"), Some("2"), Some("3")]);
    let messages: Vec<ServerJsonRpcMessage> = events
        .iter()
        .map(|one| serde_json::from_str(one.data.as_deref().expect("data")).expect("JSON-RPC"))
        .collect();
    assert!(matches!(messages[0], ServerJsonRpcMessage::Notification(_)));
    assert!(matches!(messages[1], ServerJsonRpcMessage::Notification(_)));
    assert!(matches!(messages[2], ServerJsonRpcMessage::Response(_)));
    assert!(!over.load(Ordering::Relaxed));
}

#[tokio::test]
async fn an_event_past_the_cap_fails_the_stream_and_nothing_reconnects() {
    let answer = json!({"jsonrpc":"2.0","id":1,"result":{}});
    let body = [event(1, &note(&"x".repeat(200))), event(2, &answer)].concat();
    let (url, asked) = serving(body);
    let (client, over) = client(160);

    let reply = client
        .post_message(Arc::clone(&url), ping(), None, None, HashMap::new())
        .await
        .expect("reply");
    let StreamableHttpPostResponse::Sse(stream, _) = reply else {
        panic!("not a stream: {reply:?}");
    };
    let events: Vec<_> = stream.collect().await;
    assert_eq!(events.len(), 1, "{events:?}");
    assert!(
        events[0]
            .as_ref()
            .is_err_and(|why| why.to_string().contains("over 160 bytes")),
        "{events:?}"
    );
    assert!(over.load(Ordering::Relaxed));

    let resumed = client
        .get_stream(url, None, Some("1".into()), None, HashMap::new())
        .await;
    assert!(resumed.is_err(), "a stream past the cap is not resumed");
    assert_eq!(asked.load(Ordering::Relaxed), 1);
    assert_eq!(Patience::new(over).retry(1), None);
}

#[test]
fn an_event_is_counted_from_its_first_line_to_the_blank_one_after_it() {
    let mut event = Event::new(12);
    assert!(event.fits(b"data: 1234\r\n\r\n"));
    assert!(event.fits(b"data: 1234\n\n"));
    assert!(event.fits(b": note\n: note\n"), "a comment is not kept");
    assert!(event.fits(b"data: 12\r"));
    assert!(!event.fits(b"\ndata: 1\n"));
}
