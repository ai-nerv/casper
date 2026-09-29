use serde_json::{Value, json};
use std::net::TcpStream;
use std::time::{Duration, Instant};
use tungstenite::{Message, WebSocket, protocol::WebSocketConfig};

pub(super) struct Cdp {
    socket: WebSocket<TcpStream>,
    deadline: Instant,
    next: u64,
}

impl Cdp {
    pub fn connect(endpoint: &super::Endpoint, source: &str) -> Result<Self, String> {
        let url = url::Url::parse(source).map_err(|why| why.to_string())?;
        if url.scheme() != "ws"
            || url.host_str() != endpoint.url.host_str()
            || url.port_or_known_default() != endpoint.url.port_or_known_default()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err("browser advertised a debugger outside the trusted endpoint".into());
        }
        let stream = TcpStream::connect_timeout(&endpoint.address, Duration::from_secs(3))
            .map_err(|why| format!("browser debugger unavailable: {why}"))?;
        stream
            .set_read_timeout(Some(endpoint.timeout))
            .map_err(|why| why.to_string())?;
        stream
            .set_write_timeout(Some(endpoint.timeout))
            .map_err(|why| why.to_string())?;
        let config = WebSocketConfig::default()
            .max_message_size(Some(2_000_000))
            .max_frame_size(Some(2_000_000));
        let (socket, _) = tungstenite::client::client_with_config(source, stream, Some(config))
            .map_err(|why| format!("browser debugger handshake failed: {why}"))?;
        Ok(Self {
            socket,
            deadline: Instant::now() + endpoint.timeout,
            next: 0,
        })
    }

    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.next += 1;
        let id = self.next;
        self.socket
            .send(Message::Text(
                json!({"id":id,"method":method,"params":params})
                    .to_string()
                    .into(),
            ))
            .map_err(|why| format!("browser request failed: {why}"))?;
        for _ in 0..10_000 {
            let remaining = self
                .deadline
                .checked_duration_since(Instant::now())
                .ok_or("browser operation timed out")?;
            self.socket
                .get_mut()
                .set_read_timeout(Some(remaining))
                .map_err(|why| why.to_string())?;
            let message = self
                .socket
                .read()
                .map_err(|why| format!("browser response failed: {why}"))?;
            match message {
                Message::Text(text) => {
                    let reply: Value =
                        serde_json::from_str(&text).map_err(|why| why.to_string())?;
                    if reply["id"] != id {
                        continue;
                    }
                    if let Some(error) = reply.get("error") {
                        return Err(format!("browser rejected {method}: {error}"));
                    }
                    return Ok(reply["result"].clone());
                }
                Message::Close(_) => return Err("browser debugger closed".into()),
                Message::Ping(_) => self.socket.flush().map_err(|why| why.to_string())?,
                _ => {}
            }
        }
        Err("browser event limit exceeded".into())
    }

    pub fn evaluate(&mut self, expression: String) -> Result<Value, String> {
        let reply = self.call(
            "Runtime.evaluate",
            json!({"expression":expression,"returnByValue":true,"awaitPromise":true}),
        )?;
        if let Some(error) = reply.get("exceptionDetails") {
            return Err(format!("browser DOM operation failed: {error}"));
        }
        reply["result"]
            .get("value")
            .cloned()
            .ok_or("browser returned no DOM value".into())
    }

    pub fn ready(&mut self) -> Result<(), String> {
        loop {
            if self.evaluate("document.readyState !== 'loading'".into())? == true {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn navigated(&mut self, loader: Option<&str>) -> Result<(), String> {
        if let Some(loader) = loader {
            loop {
                let frame = self.call("Page.getFrameTree", json!({}))?;
                if frame["frameTree"]["frame"]["loaderId"] == loader {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        self.ready()
    }
}
