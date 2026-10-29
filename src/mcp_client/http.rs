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
use reqwest::StatusCode;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderName, HeaderValue};
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

type Failure = StreamableHttpError<reqwest::Error>;

mod identifiers;
use identifiers::{Cipher, Kind};

/// An HTTP client whose replies are refused past `cap` bytes, setting `over` when one is.
#[derive(Clone)]
pub(super) struct Bounded {
    inner: reqwest::Client,
    cap: usize,
    over: Arc<AtomicBool>,
    auth: Option<Arc<super::oauth::Access>>,
    identifiers: Option<Arc<Cipher>>,
}

/// rmcp's reconnection backoff for an SSE stream, given up once a message has run past the cap.
pub(super) struct Patience {
    backoff: ExponentialBackoff,
    over: Arc<AtomicBool>,
    auth: Option<Arc<super::oauth::Access>>,
}

impl std::fmt::Debug for Patience {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fmt.debug_struct("Patience").finish_non_exhaustive()
    }
}

impl Patience {
    pub(super) fn new(over: Arc<AtomicBool>) -> Self {
        Self {
            backoff: ExponentialBackoff::default(),
            over,
            auth: None,
        }
    }

    pub(super) fn with_auth(mut self, auth: Option<Arc<super::oauth::Access>>) -> Self {
        self.auth = auth;
        self
    }
}

impl SseRetryPolicy for Patience {
    fn retry(&self, times: usize) -> Option<Duration> {
        if self.over.load(Ordering::Relaxed)
            || self.auth.as_ref().is_some_and(|auth| auth.stopped())
        {
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
    pub(super) const fn new(inner: reqwest::Client, cap: usize, over: Arc<AtomicBool>) -> Self {
        Self {
            inner,
            cap,
            over,
            auth: None,
            identifiers: None,
        }
    }

    pub(super) fn with_auth(
        mut self,
        auth: Option<Arc<super::oauth::Access>>,
    ) -> Result<Self, Failure> {
        self.identifiers = auth
            .as_ref()
            .map(|_| Cipher::new(self.cap).map(Arc::new))
            .transpose()
            .map_err(|_| {
                Self::authenticated_failure("authenticated MCP identifier setup failed")
            })?;
        self.auth = auth;
        Ok(self)
    }

    fn seal_identifier(&self, kind: Kind, value: String) -> Result<String, Failure> {
        let Some(cipher) = &self.identifiers else {
            return Ok(value);
        };
        cipher
            .seal(kind, &value)
            .map_err(|_| Self::authenticated_failure("authenticated MCP identifier refused"))
    }

    fn open_identifier(&self, kind: Kind, value: &str) -> Result<String, Failure> {
        let Some(cipher) = &self.identifiers else {
            return Ok(value.to_owned());
        };
        cipher
            .open(kind, value)
            .map_err(|_| Self::authenticated_failure("authenticated MCP identifier refused"))
    }

    fn identifier_header(
        &self,
        request: reqwest::RequestBuilder,
        name: &'static str,
        kind: Kind,
        value: &str,
    ) -> Result<reqwest::RequestBuilder, Failure> {
        if self.auth.is_none() {
            return Ok(request.header(name, value));
        }
        let raw = self.open_identifier(kind, value)?;
        let mut header = HeaderValue::from_str(&raw).map_err(|_| {
            Self::authenticated_failure("authenticated MCP identifier header refused")
        })?;
        header.set_sensitive(true);
        Ok(request.header(name, header))
    }

    async fn authorized(
        &self,
        uri: &str,
        original: Option<String>,
        headers: &HashMap<HeaderName, HeaderValue>,
    ) -> Result<Option<String>, Failure> {
        let Some(auth) = self.auth.as_ref().map(Arc::clone) else {
            return Ok(original);
        };
        if original.is_some() || headers.contains_key(&reqwest::header::AUTHORIZATION) {
            return Err(StreamableHttpError::UnexpectedServerResponse(
                Cow::Borrowed("conflicting MCP authentication"),
            ));
        }
        auth.destination(uri).map_err(|_| {
            StreamableHttpError::UnexpectedServerResponse(Cow::Borrowed(
                "MCP authentication destination mismatch",
            ))
        })?;
        auth.token().await.map(Some).map_err(|error| {
            StreamableHttpError::UnexpectedServerResponse(Cow::Owned(error.to_string()))
        })
    }

    fn refused(&self) -> Failure {
        self.over.store(true, Ordering::Relaxed);
        StreamableHttpError::UnexpectedServerResponse(Cow::Owned(format!(
            "MCP HTTP message over {} bytes",
            self.cap
        )))
    }

    async fn admitted<T>(
        &self,
        future: impl std::future::Future<Output = Result<T, Failure>>,
    ) -> Result<T, Failure> {
        let Some(auth) = &self.auth else {
            return future.await;
        };
        tokio::select! {
            biased;
            _ = auth.cancelled() => Err(StreamableHttpError::UnexpectedServerResponse(Cow::Borrowed("MCP authentication cancelled"))),
            result = future => result.map_err(|_| Self::authenticated_failure("authenticated MCP transport failed")),
        }
    }

    fn authenticated_failure(message: &'static str) -> Failure {
        StreamableHttpError::UnexpectedServerResponse(Cow::Borrowed(message))
    }

    fn unexpected_content(&self, kind: Option<String>) -> Failure {
        if self.auth.is_some() {
            Self::authenticated_failure("authenticated MCP response has unexpected content type")
        } else {
            StreamableHttpError::UnexpectedContentType(kind)
        }
    }

    /// `request` with the headers every request carries. A custom header may not stand in for
    /// one of the transport's own.
    fn headed(
        request: reqwest::RequestBuilder,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<reqwest::RequestBuilder, Failure> {
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
    async fn whole(&self, mut response: reqwest::Response) -> Result<Vec<u8>, Failure> {
        if response
            .content_length()
            .is_some_and(|length| length > self.cap as u64)
        {
            return Err(self.refused());
        }
        let mut body = Vec::new();
        while let Some(chunk) = self.admitted(async { Ok(response.chunk().await?) }).await? {
            if body.len().saturating_add(chunk.len()) > self.cap {
                return Err(self.refused());
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    /// An SSE body as the events it carries, ending in a failure at the first event past the cap.
    fn events(&self, response: reqwest::Response) -> BoxedSseResponse {
        let (cap, over) = (self.cap, Arc::clone(&self.over));
        let auth = self.auth.clone();
        let bytes = response
            .bytes_stream()
            .take_until(async move {
                match auth {
                    Some(auth) => auth.cancelled().await,
                    None => std::future::pending().await,
                }
            })
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
        let auth = self.auth.clone();
        let identifiers = self.identifiers.clone();
        SseStream::from_bytes_stream(bytes)
            .scan(false, move |ended, result| {
                let Some(auth) = &auth else {
                    return futures::future::ready(Some(result));
                };
                if *ended {
                    return futures::future::ready(None);
                }
                let result = match result {
                    Ok(mut event) => {
                        let message = matches!(event.event.as_deref(), None | Some("" | "message"));
                        if !message
                            || event
                                .data
                                .as_deref()
                                .is_some_and(|data| data.trim().is_empty())
                        {
                            event.data = None;
                        }
                        if event.data.as_deref().is_some_and(|data| {
                            !data.trim().is_empty()
                                && serde_json::from_str::<ServerJsonRpcMessage>(data).is_err()
                        }) {
                            *ended = true;
                            auth.stop();
                            Err(sse_stream::Error::Body(Box::new(std::io::Error::other(
                                "authenticated MCP SSE reply is not JSON-RPC",
                            ))))
                        } else {
                            match event
                                .id
                                .take()
                                .map(|id| {
                                    identifiers
                                        .as_ref()
                                        .ok_or(identifiers::Invalid)?
                                        .seal(Kind::Cursor, &id)
                                })
                                .transpose()
                            {
                                Ok(id) => {
                                    event.id = id;
                                    Ok(event)
                                }
                                Err(_) => {
                                    *ended = true;
                                    auth.stop();
                                    Err(sse_stream::Error::Body(Box::new(std::io::Error::other(
                                        "authenticated MCP identifier refused",
                                    ))))
                                }
                            }
                        }
                    }
                    Err(_) => {
                        *ended = true;
                        Err(sse_stream::Error::Body(Box::new(std::io::Error::other(
                            "authenticated MCP SSE transport failed",
                        ))))
                    }
                };
                futures::future::ready(Some(result))
            })
            .boxed()
    }
}

impl StreamableHttpClient for Bounded {
    type Error = reqwest::Error;

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
        let auth_header = self.authorized(&uri, auth_header, &custom_headers).await?;
        let mut request = Self::headed(self.inner.post(uri.as_ref()), auth_header, custom_headers)?;
        let attached = session_id.is_some();
        if let Some(session) = session_id {
            request =
                self.identifier_header(request, HEADER_SESSION_ID, Kind::Session, &session)?;
        }
        let response = self
            .admitted(async { Ok(request.json(&message).send().await?) })
            .await?;
        let status = response.status();
        if self.auth.is_some() && !status.is_success() {
            if let Some(auth) = &self.auth {
                auth.stop();
            }
            return Err(StreamableHttpError::UnexpectedServerResponse(
                Cow::Borrowed(if status == StatusCode::UNAUTHORIZED {
                    "authentication_required"
                } else {
                    "authenticated MCP request failed"
                }),
            ));
        }
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
        let kind = header(CONTENT_TYPE.as_str());
        let session = header(HEADER_SESSION_ID)
            .map(|value| self.seal_identifier(Kind::Session, value))
            .transpose()?;
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
            return Err(self.unexpected_content(kind));
        }
        let body = self.whole(response).await?;
        match serde_json::from_slice::<ServerJsonRpcMessage>(&body) {
            Ok(reply) => Ok(StreamableHttpPostResponse::Json(reply, session)),
            Err(_) if !asks => Ok(StreamableHttpPostResponse::Accepted),
            Err(_) if self.auth.is_some() => Err(Self::authenticated_failure(
                "authenticated MCP HTTP reply is not JSON-RPC",
            )),
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
        let auth_header = self.authorized(&uri, auth_header, &custom_headers).await?;
        if self.auth.is_some() {
            let request =
                Self::headed(self.inner.delete(uri.as_ref()), auth_header, custom_headers)?;
            let request =
                self.identifier_header(request, HEADER_SESSION_ID, Kind::Session, &session_id)?;
            return self
                .admitted(async {
                    let response = request.send().await?;
                    if response.status() != StatusCode::METHOD_NOT_ALLOWED {
                        response.error_for_status()?;
                    }
                    Ok(())
                })
                .await;
        }
        self.admitted(
            self.inner
                .delete_session(uri, session_id, auth_header, custom_headers),
        )
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
        let auth_header = self.authorized(&uri, auth_header, &custom_headers).await?;
        let mut request = Self::headed(self.inner.get(uri.as_ref()), auth_header, custom_headers)?;
        if let Some(session) = session_id {
            request =
                self.identifier_header(request, HEADER_SESSION_ID, Kind::Session, &session)?;
        }
        if let Some(last) = last_event_id {
            request = self.identifier_header(request, HEADER_LAST_EVENT_ID, Kind::Cursor, &last)?;
        }
        let response = self.admitted(async { Ok(request.send().await?) }).await?;
        if self.auth.is_some()
            && matches!(
                response.status(),
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
            )
        {
            if let Some(auth) = &self.auth {
                auth.stop();
            }
            return Err(StreamableHttpError::UnexpectedServerResponse(
                Cow::Borrowed("authentication_required"),
            ));
        }
        if response.status() == StatusCode::METHOD_NOT_ALLOWED {
            return Err(StreamableHttpError::ServerDoesNotSupportSse);
        }
        let response = self
            .admitted(async { Ok(response.error_for_status()?) })
            .await?;
        let kind = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        if !kind.as_deref().is_some_and(|kind| {
            kind.starts_with(EVENT_STREAM_MIME_TYPE) || kind.starts_with(JSON_MIME_TYPE)
        }) {
            return Err(self.unexpected_content(kind));
        }
        Ok(self.events(response))
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod auth_tests;

#[cfg(test)]
mod identifier_tests;
