//! A streamable-HTTP server's end of a session, every reply held to a cap: a JSON body whole, and
//! an SSE stream, a POST's or the GET one, one event at a time. An SSE stream reaches rmcp whole,
//! so the notifications and requests on it are delivered, and a stream that closes before its
//! response is resumed with `Last-Event-ID`. Once one event runs past the cap, nothing more is
//! read: the stream fails, and neither a reconnection nor a retry is made.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::StreamExt as _;
use mcp_http::StatusCode;
use mcp_http::header::{ACCEPT, CONTENT_TYPE, HeaderName, HeaderValue};
use rmcp::model::{ClientJsonRpcMessage, ServerJsonRpcMessage};
use rmcp::transport::common::client_side_sse::{
    BoxedSseResponse, ExponentialBackoff, SseRetryPolicy,
};
use rmcp::transport::common::http_header::{
    EVENT_STREAM_MIME_TYPE, HEADER_LAST_EVENT_ID, HEADER_SESSION_ID, JSON_MIME_TYPE,
};
use rmcp::transport::streamable_http_client::{
    StreamableHttpClient, StreamableHttpError, StreamableHttpPostResponse,
};
use sse_stream::SseStream;

type Failure = StreamableHttpError<mcp_http::Error>;

/// An HTTP client whose replies are refused past `cap` bytes, setting `over` when one is.
#[derive(Clone)]
pub(super) struct Bounded {
    inner: mcp_http::Client,
    cap: usize,
    over: Arc<AtomicBool>,
}

/// rmcp's reconnection backoff for an SSE stream, given up once a message has run past the cap.
#[derive(Debug)]
pub(super) struct Patience {
    backoff: ExponentialBackoff,
    over: Arc<AtomicBool>,
}

impl Patience {
    pub(super) fn new(over: Arc<AtomicBool>) -> Self {
        Self {
            backoff: ExponentialBackoff::default(),
            over,
        }
    }
}

impl SseRetryPolicy for Patience {
    fn retry(&self, times: usize) -> Option<Duration> {
        if self.over.load(Ordering::Relaxed) {
            return None;
        }
        self.backoff.retry(times)
    }
}

/// How much of one SSE event has arrived, counted as rmcp counts it: the lines the event keeps,
/// plus the line still arriving. A comment line counts only while it arrives; a blank line ends
/// the event.
struct Event {
    cap: usize,
    kept: usize,
    line: usize,
    comment: bool,
    carriage: bool,
    ended: bool,
}

impl Event {
    const fn new(cap: usize) -> Self {
        Self {
            cap,
            kept: 0,
            line: 0,
            comment: false,
            carriage: false,
            ended: false,
        }
    }

    /// Whether the event is still within the cap once `chunk` has arrived.
    fn fits(&mut self, chunk: &[u8]) -> bool {
        for &byte in chunk {
            if std::mem::take(&mut self.carriage) && byte == b'\n' {
                continue;
            }
            if matches!(byte, b'\r' | b'\n') {
                self.kept = match (self.line, self.comment) {
                    (0, _) => 0,
                    (_, true) => self.kept,
                    (line, false) => self.kept.saturating_add(line).saturating_add(1),
                };
                (self.line, self.comment, self.carriage) = (0, false, byte == b'\r');
            } else {
                if self.line == 0 {
                    self.comment = byte == b':';
                }
                self.line = self.line.saturating_add(1);
            }
            if self.kept.saturating_add(self.line) > self.cap {
                return false;
            }
        }
        true
    }
}

impl Bounded {
    pub(super) const fn new(inner: mcp_http::Client, cap: usize, over: Arc<AtomicBool>) -> Self {
        Self { inner, cap, over }
    }

    fn refused(&self) -> Failure {
        self.over.store(true, Ordering::Relaxed);
        StreamableHttpError::UnexpectedServerResponse(Cow::Owned(format!(
            "MCP HTTP message over {} bytes",
            self.cap
        )))
    }

    /// `request` with the headers every request carries. A custom header may not stand in for
    /// one of the transport's own.
    fn headed(
        request: mcp_http::RequestBuilder,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<mcp_http::RequestBuilder, Failure> {
        let mut request = request.header(
            ACCEPT,
            format!("{EVENT_STREAM_MIME_TYPE}, {JSON_MIME_TYPE}"),
        );
        if let Some(token) = auth_header {
            request = request.bearer_auth(token);
        }
        for (name, value) in custom_headers {
            if ["accept", HEADER_SESSION_ID, HEADER_LAST_EVENT_ID]
                .iter()
                .any(|reserved| name.as_str().eq_ignore_ascii_case(reserved))
            {
                return Err(StreamableHttpError::ReservedHeaderConflict(
                    name.to_string(),
                ));
            }
            request = request.header(name, value);
        }
        Ok(request)
    }

    /// A body read whole.
    async fn whole(&self, mut response: mcp_http::Response) -> Result<Vec<u8>, Failure> {
        if response
            .content_length()
            .is_some_and(|length| length > self.cap as u64)
        {
            return Err(self.refused());
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if body.len().saturating_add(chunk.len()) > self.cap {
                return Err(self.refused());
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    /// An SSE body as the events it carries, ending in a failure at the first event past the cap.
    fn events(&self, response: mcp_http::Response) -> BoxedSseResponse {
        let (cap, over) = (self.cap, Arc::clone(&self.over));
        let bytes = response
            .bytes_stream()
            .scan(Event::new(cap), move |event, chunk| {
                let passed = match chunk {
                    _ if event.ended => None,
                    Ok(chunk) if event.fits(&chunk) => Some(Ok(chunk)),
                    Ok(_) => {
                        event.ended = true;
                        over.store(true, Ordering::Relaxed);
                        Some(Err(std::io::Error::other(format!(
                            "MCP SSE event over {cap} bytes"
                        ))))
                    }
                    Err(why) => {
                        event.ended = true;
                        Some(Err(std::io::Error::other(why)))
                    }
                };
                futures::future::ready(passed)
            });
        SseStream::from_bytes_stream(bytes).boxed()
    }
}

impl StreamableHttpClient for Bounded {
    type Error = mcp_http::Error;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, Failure> {
        self.post_message_with_max_sse_event_size(
            uri,
            message,
            session_id,
            auth_header,
            custom_headers,
            self.cap,
        )
        .await
    }

    async fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        _: usize,
    ) -> Result<StreamableHttpPostResponse, Failure> {
        let mut request = Self::headed(self.inner.post(uri.as_ref()), auth_header, custom_headers)?;
        let attached = session_id.is_some();
        if let Some(session) = session_id {
            request = request.header(HEADER_SESSION_ID, session.as_ref());
        }
        let response = request.json(&message).send().await?;
        let status = response.status();
        if matches!(status, StatusCode::ACCEPTED | StatusCode::NO_CONTENT) {
            return Ok(StreamableHttpPostResponse::Accepted);
        }
        if status == StatusCode::NOT_FOUND && attached {
            return Err(StreamableHttpError::SessionExpired);
        }
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        let (kind, session) = (header(CONTENT_TYPE.as_str()), header(HEADER_SESSION_ID));
        let json = kind
            .as_deref()
            .is_some_and(|kind| kind.starts_with(JSON_MIME_TYPE));
        let asks = matches!(message, ClientJsonRpcMessage::Request(_));
        if !status.is_success() {
            let body = self.whole(response).await?;
            return match serde_json::from_slice::<ServerJsonRpcMessage>(&body) {
                Ok(error @ ServerJsonRpcMessage::Error(_)) if json => {
                    Ok(StreamableHttpPostResponse::Json(error, session))
                }
                _ => Err(StreamableHttpError::UnexpectedServerResponse(Cow::Owned(
                    format!("HTTP {status}: {}", String::from_utf8_lossy(&body)),
                ))),
            };
        }
        if !asks && response.content_length() == Some(0) {
            return Ok(StreamableHttpPostResponse::Accepted);
        }
        if kind
            .as_deref()
            .is_some_and(|kind| kind.starts_with(EVENT_STREAM_MIME_TYPE))
        {
            return Ok(StreamableHttpPostResponse::Sse(
                self.events(response),
                session,
            ));
        }
        if !json {
            return Err(StreamableHttpError::UnexpectedContentType(kind));
        }
        let body = self.whole(response).await?;
        match serde_json::from_slice::<ServerJsonRpcMessage>(&body) {
            Ok(reply) => Ok(StreamableHttpPostResponse::Json(reply, session)),
            Err(_) if !asks => Ok(StreamableHttpPostResponse::Accepted),
            Err(why) => Err(StreamableHttpError::UnexpectedServerResponse(Cow::Owned(
                format!("MCP HTTP reply is not JSON-RPC: {why}"),
            ))),
        }
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), Failure> {
        self.inner
            .delete_session(uri, session_id, auth_header, custom_headers)
            .await
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxedSseResponse, Failure> {
        self.get_stream_with_max_sse_event_size(
            uri,
            session_id,
            last_event_id,
            auth_header,
            custom_headers,
            self.cap,
        )
        .await
    }

    async fn get_stream_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
        _: usize,
    ) -> Result<BoxedSseResponse, Failure> {
        if self.over.load(Ordering::Relaxed) {
            return Err(self.refused());
        }
        let mut request = Self::headed(self.inner.get(uri.as_ref()), auth_header, custom_headers)?;
        if let Some(session) = session_id {
            request = request.header(HEADER_SESSION_ID, session.as_ref());
        }
        if let Some(last) = last_event_id {
            request = request.header(HEADER_LAST_EVENT_ID, last);
        }
        let response = request.send().await?;
        if response.status() == StatusCode::METHOD_NOT_ALLOWED {
            return Err(StreamableHttpError::ServerDoesNotSupportSse);
        }
        let response = response.error_for_status()?;
        let kind = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        if !kind.as_deref().is_some_and(|kind| {
            kind.starts_with(EVENT_STREAM_MIME_TYPE) || kind.starts_with(JSON_MIME_TYPE)
        }) {
            return Err(StreamableHttpError::UnexpectedContentType(kind));
        }
        Ok(self.events(response))
    }
}

#[cfg(test)]
mod tests;
