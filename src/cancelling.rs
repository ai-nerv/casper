use std::sync::atomic::{AtomicBool, Ordering};
use std::{cell::RefCell, os::unix::net::UnixStream};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reason {
    Interrupted,
    Disconnected,
}

/// Listen for `SIGTERM` on a background thread so `casper run` can stop a running command and
/// still emit partial output before exiting.
pub fn arm_sigterm() {
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
    let _ = std::thread::Builder::new()
        .name("casper-sigterm".into())
        .spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                let _ = ready_tx.send(());
                return;
            };
            rt.block_on(async move {
                let Ok(mut term) =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                else {
                    let _ = ready_tx.send(());
                    return;
                };
                let _ = ready_tx.send(());
                if term.recv().await.is_some() {
                    INTERRUPTED.store(true, Ordering::Release);
                }
            });
        });
    let _ = ready_rx.recv_timeout(std::time::Duration::from_millis(200));
}

enum Watch {
    Off,
    Peer(UnixStream),
    Unavailable,
}

thread_local! {
    static CURRENT: RefCell<Watch> = const { RefCell::new(Watch::Off) };
}

struct Scope(Option<Watch>);

impl Drop for Scope {
    fn drop(&mut self) {
        if let Some(previous) = self.0.take() {
            CURRENT.set(previous);
        }
    }
}

pub(crate) fn with_peer<T>(peer: &UnixStream, work: impl FnOnce() -> T) -> T {
    let watch = peer.try_clone().map_or(Watch::Unavailable, Watch::Peer);
    let _scope = Scope(Some(CURRENT.replace(watch)));
    work()
}

fn peer_state(peer: &UnixStream) -> Option<Reason> {
    use rustix::event::{PollFd, PollFlags, Timespec};
    let mut fds = [PollFd::new(peer, PollFlags::IN)];
    let polled = rustix::event::poll(&mut fds, Some(&Timespec::default()));
    let Ok(ready) = polled else {
        return Some(Reason::Disconnected);
    };
    if ready == 0 {
        return None;
    }
    let revents = fds.first().map_or(PollFlags::empty(), PollFd::revents);
    if revents.intersects(PollFlags::HUP | PollFlags::ERR | PollFlags::NVAL) {
        return Some(Reason::Disconnected);
    }
    if revents.contains(PollFlags::IN) {
        let mut byte = [0_u8; 1];
        let flags = rustix::net::RecvFlags::PEEK | rustix::net::RecvFlags::DONTWAIT;
        if matches!(rustix::net::recv(peer, &mut byte, flags), Ok((0, _))) {
            return Some(Reason::Interrupted);
        }
    }
    None
}

pub(crate) fn status() -> Option<Reason> {
    if INTERRUPTED.load(Ordering::Acquire) {
        return Some(Reason::Interrupted);
    }
    CURRENT.with_borrow(|watch| match watch {
        Watch::Off => None,
        Watch::Unavailable => Some(Reason::Disconnected),
        Watch::Peer(peer) => peer_state(peer),
    })
}

pub(crate) fn abandoned() -> bool {
    status().is_some()
}

pub(crate) struct Captured(Watch);

pub(crate) fn capture() -> Captured {
    Captured(CURRENT.with_borrow(|watch| match watch {
        Watch::Off => Watch::Off,
        Watch::Unavailable => Watch::Unavailable,
        Watch::Peer(peer) => peer.try_clone().map_or(Watch::Unavailable, Watch::Peer),
    }))
}

impl Captured {
    pub(crate) fn abandoned(&self) -> bool {
        if INTERRUPTED.load(Ordering::Acquire) {
            return true;
        }
        match &self.0 {
            Watch::Off => false,
            Watch::Unavailable => true,
            Watch::Peer(peer) => peer_state(peer).is_some(),
        }
    }

    pub(crate) async fn cancelled(&self) {
        if matches!(self.0, Watch::Off) {
            std::future::pending::<()>().await;
        }
        while !self.abandoned() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_scopes_restore_after_nesting_and_unwind() {
        let (outer, closed) = UnixStream::pair().expect("outer");
        let (inner, _open) = UnixStream::pair().expect("inner");
        closed
            .shutdown(std::net::Shutdown::Both)
            .expect("full close");
        drop(closed);
        assert!(!abandoned());
        with_peer(&outer, || {
            assert!(abandoned());
            let panic = std::panic::catch_unwind(|| {
                with_peer(&inner, || {
                    assert!(!abandoned());
                    panic!("scope unwind");
                })
            });
            assert!(panic.is_err());
            assert!(abandoned());
        });
        assert!(!abandoned());
    }

    #[test]
    fn disconnect_reaps_running_commands_and_refuses_followup_launches() {
        use std::time::{Duration, Instant};
        let root = crate::scratch::Scratch::new("casper", "cancel-running");
        let ready = root.join("ready");
        let pid = root.join("pid");
        let forbidden = root.join("forbidden");
        let script = format!(
            "printf '%s' $$ > '{}'; : > '{}'; exec sleep 30",
            pid.display(),
            ready.display()
        );
        let followup = format!("printf forbidden > '{}'", forbidden.display());
        let (ours, peer) = UnixStream::pair().expect("connection");
        let (done, answer) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            with_peer(&ours, || {
                let result = crate::running::run("sh", &["-c".into(), script]);
                let next = crate::running::run("sh", &["-c".into(), followup.clone()]);
                let binary = crate::running::binary("sh", &["-c".into(), followup.clone()], 128);
                let piped =
                    crate::running::piped("sh", followup.into_bytes(), 128, Duration::from_secs(1));
                done.send((result, next, binary, piped)).expect("results");
            })
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        let started = ready.exists();
        drop(peer);
        let (result, next, binary, piped) = answer
            .recv_timeout(Duration::from_secs(2))
            .expect("bounded cancellation");
        worker.join().expect("command worker");
        assert!(started);
        assert_ne!(result.code, 0);
        assert!(result.err.contains("caller disconnected"));
        assert_ne!(next.code, 0);
        assert!(binary.is_err());
        assert!(piped.is_err());
        assert!(!forbidden.exists());
        let pid = std::fs::read_to_string(pid).expect("native PID");
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
        assert_eq!(crate::running::run("true", &[]).code, 0);
    }

    #[test]
    fn half_close_cancels_and_preserves_partial_output() {
        use std::net::Shutdown;
        use std::time::{Duration, Instant};
        let root = crate::scratch::Scratch::new("casper", "half-close");
        let ready = root.join("ready");
        let script = format!(
            "printf 'partial-line-1\\n'; : > '{}'; exec sleep 30",
            ready.display()
        );
        let (ours, peer) = UnixStream::pair().expect("connection");
        let (done, answer) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            with_peer(&ours, || {
                let result = crate::running::run("sh", &["-c".into(), script]);
                done.send(result).expect("result");
            })
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(ready.exists());
        std::thread::sleep(Duration::from_millis(50));
        peer.shutdown(Shutdown::Write).expect("half-close");
        let result = answer
            .recv_timeout(Duration::from_secs(3))
            .expect("cancelled result");
        worker.join().expect("worker");
        assert_ne!(result.code, 0);
        assert!(result.out.contains("partial-line-1"), "{}", result.out);
        assert!(result.err.contains("cancelled"), "{}", result.err);
    }
}
