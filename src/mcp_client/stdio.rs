//! A stdio server's end of a session: started inside the jail, its environment handed to the
//! program alone, and every line it writes held to a cap.

use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, ReadBuf};
use tokio::process::{Child, ChildStdin, ChildStdout};

/// A server a session holds: killed when the session lets it go, and on the Landlock path what it
/// changed that bubblewrap would have sealed is put back.
pub(super) struct Served(
    pub crate::jail::Prepared,
    pub Child,
    Option<std::os::fd::OwnedFd>,
);

impl Drop for Served {
    fn drop(&mut self) {
        let supervised = self.2.take().is_some();
        if !supervised {
            if let Some(group) = self
                .1
                .id()
                .and_then(|pid| i32::try_from(pid).ok())
                .and_then(rustix::process::Pid::from_raw)
            {
                let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
            }
            let _ = self.1.start_kill();
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while matches!(self.1.try_wait(), Ok(None)) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        for line in self.0.restored() {
            crate::noted!("mcp: {line}");
        }
    }
}

/// A started server, and the two pipes its session is spoken over.
pub(super) struct Launched {
    pub server: Served,
    pub read: Capped<ChildStdout>,
    pub write: ChildStdin,
}

/// Start `command` as the jail prepares it. `env` reaches the server and never bubblewrap; the
/// child is killed when the session drops it.
pub(super) fn launch(
    jail: &crate::jail::Jail,
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    over: Arc<AtomicBool>,
) -> Result<Launched, String> {
    let env: Vec<_> = env
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let mut prepared = jail
        .prepare(command, args, None, &[], false)
        .map_err(|why| format!("MCP command: {why}"))?;
    prepared.inner(&env);
    spawn(prepared, over)
}

fn spawn(mut prepared: crate::jail::Prepared, over: Arc<AtomicBool>) -> Result<Launched, String> {
    let (mut command, lifetime) = prepared
        .mcp_command()
        .map_err(|why| format!("MCP supervision: {why}"))?;
    let child = command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(lifetime.is_none())
        .spawn()
        .map_err(|why| format!("MCP stdio: {why}"))?;
    let mut server = Served(prepared, child, lifetime);
    let read = server.1.stdout.take().ok_or("MCP stdio: no output pipe")?;
    let write = server.1.stdin.take().ok_or("MCP stdio: no input pipe")?;
    Ok(Launched {
        server,
        read: Capped::new(read, super::REPLY, over),
        write,
    })
}

/// A server's standard output, refused once one line runs past `cap` bytes. rmcp reads a stdio
/// message as one whole line and sets no limit of its own.
pub(super) struct Capped<R> {
    inner: R,
    cap: usize,
    line: usize,
    over: Arc<AtomicBool>,
}

impl<R> Capped<R> {
    pub(super) const fn new(inner: R, cap: usize, over: Arc<AtomicBool>) -> Self {
        Self {
            inner,
            cap,
            line: 0,
            over,
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for Capped<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if this.over.load(Ordering::Relaxed) {
            return Poll::Ready(Err(overflow(this.cap)));
        }
        let before = buf.filled().len();
        std::task::ready!(Pin::new(&mut this.inner).poll_read(cx, buf))?;
        let read = buf.filled().get(before..).unwrap_or_default();
        for (n, part) in read.split(|byte| *byte == b'\n').enumerate() {
            if n > 0 {
                this.line = 0;
            }
            this.line = this.line.saturating_add(part.len());
            if this.line > this.cap {
                // An error leaves the buffer as it found it.
                buf.set_filled(before);
                this.over.store(true, Ordering::Relaxed);
                return Poll::Ready(Err(overflow(this.cap)));
            }
        }
        Poll::Ready(Ok(()))
    }
}

fn overflow(cap: usize) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("MCP stdio message over {cap} bytes"),
    )
}

#[cfg(test)]
mod ownership;

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt as _;

    async fn drained(input: &[u8], cap: usize) -> (std::io::Result<usize>, bool) {
        let over = Arc::new(AtomicBool::new(false));
        let mut reader = Capped::new(input, cap, Arc::clone(&over));
        let mut out = Vec::new();
        let read = reader.read_to_end(&mut out).await;
        (read, over.load(Ordering::Relaxed))
    }

    #[tokio::test]
    async fn lines_up_to_the_cap_pass_and_the_count_restarts_at_each_newline() {
        let (read, over) = drained(b"12345678\n12345678\n12345678", 8).await;
        assert_eq!(read.ok(), Some(26));
        assert!(!over);
    }

    #[tokio::test]
    async fn a_line_one_byte_past_the_cap_is_refused_and_stays_refused() {
        let (read, over) = drained(b"1234\n123456789\nok\n", 8).await;
        assert_eq!(
            read.map_err(|why| why.kind()).err(),
            Some(std::io::ErrorKind::InvalidData)
        );
        assert!(over);
        let shut = Arc::new(AtomicBool::new(true));
        let mut reader = Capped::new(&b"ok\n"[..], 8, shut);
        let mut out = Vec::new();
        assert!(reader.read_to_end(&mut out).await.is_err());
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn a_line_without_a_newline_is_counted_across_reads() {
        let (sender, receiver) = tokio::io::duplex(4);
        let writer = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt as _;
            let mut sender = sender;
            for _ in 0..4 {
                if sender.write_all(b"abc").await.is_err() {
                    break;
                }
            }
        });
        let over = Arc::new(AtomicBool::new(false));
        let mut reader = Capped::new(receiver, 10, Arc::clone(&over));
        let mut out = Vec::new();
        assert!(reader.read_to_end(&mut out).await.is_err());
        assert!(over.load(Ordering::Relaxed));
        writer.abort();
    }
}
