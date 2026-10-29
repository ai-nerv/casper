use super::{Discovery, Error, pkce};
use std::collections::BTreeMap;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use url::Url;

pub(super) struct Callback {
    listener: TcpListener,
    pub redirect: String,
    host: String,
}

impl Callback {
    pub async fn bind(port: u16) -> Result<Self, Error> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .map_err(|_| Error::Callback)?;
        let host = listener
            .local_addr()
            .map_err(|_| Error::Callback)?
            .to_string();
        Ok(Self {
            redirect: format!("http://{host}/oauth/callback"),
            listener,
            host,
        })
    }

    pub async fn receive(self, state: &str, discovery: &Discovery) -> Result<String, Error> {
        let (mut socket, peer) = self.listener.accept().await.map_err(|_| Error::Callback)?;
        if !peer.ip().is_loopback() {
            return Err(Error::Callback);
        }
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                if request.len() >= 8192 { return Err(Error::Callback); }
                request.push(socket.read_u8().await.map_err(|_| Error::Callback)?);
            }
            let result = self.parse(&request, state, discovery);
            let reply: &[u8] = if result.is_ok() {
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: 39\r\n\r\nAuthorization received. Return to CLI.\n"
            } else {
                b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: 24\r\n\r\nAuthorization rejected.\n"
            };
            socket.write_all(reply).await.map_err(|_| Error::Callback)?;
            socket.shutdown().await.map_err(|_| Error::Callback)?;
            result
        }).await.map_err(|_| Error::Timeout)?
    }

    fn parse(&self, bytes: &[u8], state: &str, discovery: &Discovery) -> Result<String, Error> {
        let request = std::str::from_utf8(bytes).map_err(|_| Error::Callback)?;
        let mut lines = request.split("\r\n");
        let first = lines.next().ok_or(Error::Callback)?;
        let mut parts = first.split(' ');
        if parts.next() != Some("GET") {
            return Err(Error::Callback);
        }
        let path = parts.next().ok_or(Error::Callback)?;
        if parts.next() != Some("HTTP/1.1")
            || parts.next().is_some()
            || !path.starts_with("/oauth/callback?")
        {
            return Err(Error::Callback);
        }
        let mut host = None;
        for line in lines.take_while(|line| !line.is_empty()) {
            let (name, value) = line.split_once(':').ok_or(Error::Callback)?;
            if name.eq_ignore_ascii_case("host") && host.replace(value.trim()).is_some() {
                return Err(Error::Callback);
            }
            if name.eq_ignore_ascii_case("transfer-encoding")
                || (name.eq_ignore_ascii_case("content-length") && value.trim() != "0")
            {
                return Err(Error::Callback);
            }
        }
        if host != Some(self.host.as_str()) {
            return Err(Error::Callback);
        }
        let url =
            Url::parse(&format!("http://{}{path}", self.host)).map_err(|_| Error::Callback)?;
        if url.path() != "/oauth/callback" || url.fragment().is_some() {
            return Err(Error::Callback);
        }
        let mut parameters = BTreeMap::new();
        for (name, value) in url.query_pairs() {
            if parameters.len() >= 16
                || parameters
                    .insert(name.into_owned(), value.into_owned())
                    .is_some()
            {
                return Err(Error::Callback);
            }
        }
        if !parameters
            .get("state")
            .is_some_and(|value| pkce::equal(value, state))
        {
            return Err(Error::Callback);
        }
        match parameters.get("iss") {
            Some(issuer) if issuer != &discovery.issuer => return Err(Error::Callback),
            None if discovery.require_response_issuer => return Err(Error::Callback),
            _ => {}
        }
        if parameters.contains_key("error") {
            return Err(Error::Required);
        }
        let code = parameters.remove("code").ok_or(Error::Callback)?;
        if code.is_empty()
            || code.len() > 4096
            || code.bytes().any(|byte| byte < 0x21 || byte == 0x7f)
        {
            return Err(Error::Callback);
        }
        Ok(code)
    }
}

#[cfg(test)]
mod tests;
