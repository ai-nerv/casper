use super::*;

pub(super) const DISCONNECTED: &str =
    "MCP caller disconnected; request cancelled; remote effects may have occurred; not retried";

pub(super) fn check(caller: &crate::cancelling::Captured) -> Result<(), String> {
    if caller.abandoned() {
        Err(DISCONNECTED.into())
    } else {
        Ok(())
    }
}

pub(super) fn lock<'a, T>(
    mutex: &'a Mutex<T>,
    caller: &crate::cancelling::Captured,
    mut active: impl FnMut() -> Result<(), String>,
) -> Result<std::sync::MutexGuard<'a, T>, String> {
    loop {
        check(caller)?;
        active()?;
        match mutex.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(std::sync::TryLockError::Poisoned(error)) => return Ok(error.into_inner()),
            Err(std::sync::TryLockError::WouldBlock) => {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

impl Session {
    pub(super) fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        self.trace.closing();
        self.service.cancellation_token().cancel();
        let server = self
            .server
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        drop(server);
    }
}

pub(super) async fn run<T>(
    caller: &crate::cancelling::Captured,
    work: impl std::future::Future<Output = Result<T, String>>,
) -> Result<T, String> {
    tokio::select! {
        biased;
        () = caller.cancelled() => Err(DISCONNECTED.into()),
        result = work => {
            if caller.abandoned() { Err(DISCONNECTED.into()) } else { result }
        }
    }
}

#[cfg(test)]
mod http_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    #[tokio::test]
    async fn closed_peer_refuses_ready_work_without_polling_it() {
        let (ours, peer) = UnixStream::pair().expect("peer");
        let caller = crate::cancelling::with_peer(&ours, crate::cancelling::capture);
        peer.shutdown(std::net::Shutdown::Both).expect("full close");
        drop(peer);
        let executed = AtomicBool::new(false);
        let result = run(&caller, async {
            executed.store(true, Ordering::Relaxed);
            Ok(())
        })
        .await;
        assert_eq!(result.expect_err("closed peer"), DISCONNECTED);
        assert!(!executed.load(Ordering::Relaxed));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn captured_peer_crosses_threads_and_cancels_pending_work() {
        let (ours, peer) = UnixStream::pair().expect("peer");
        let caller = crate::cancelling::with_peer(&ours, crate::cancelling::capture);
        let (began, started) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            run(&caller, async {
                began.send(()).expect("started");
                std::future::pending::<Result<(), String>>().await
            })
            .await
        });
        started.await.expect("actual pending work");
        drop(peer);
        let result = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("bounded cancellation")
            .expect("worker joined");
        assert_eq!(result.expect_err("cancelled work"), DISCONNECTED);
    }

    #[tokio::test]
    async fn write_half_close_preserves_the_call_and_unobserved_work_still_runs() {
        let (ours, peer) = UnixStream::pair().expect("peer");
        let caller = crate::cancelling::with_peer(&ours, crate::cancelling::capture);
        peer.shutdown(std::net::Shutdown::Write)
            .expect("half close");
        assert_eq!(run(&caller, async { Ok(7) }).await.expect("half closed"), 7);
        let off = crate::cancelling::capture();
        assert_eq!(run(&off, async { Ok(11) }).await.expect("unobserved"), 11);
        let closing = run(&caller, async {
            peer.shutdown(std::net::Shutdown::Both)
                .expect("observable full close");
            drop(peer);
            Ok(())
        })
        .await;
        assert_eq!(closing.expect_err("completion race"), DISCONNECTED);
    }

    #[test]
    fn pool_wait_cancels_without_acquiring_or_changing_the_held_pool() {
        for closed in [false, true] {
            let mutex = Arc::new(Mutex::new(41));
            let held = mutex.lock().expect("held pool");
            let (ours, peer) = UnixStream::pair().expect("peer");
            let caller = crate::cancelling::with_peer(&ours, crate::cancelling::capture);
            let stop = Arc::new(AtomicBool::new(false));
            let waiting = mutex.clone();
            let stopping = stop.clone();
            let (entered, started) = std::sync::mpsc::sync_channel(1);
            let (send, result) = std::sync::mpsc::sync_channel(1);
            let worker = std::thread::spawn(move || {
                let mut entered = Some(entered);
                let result = lock(&waiting, &caller, || {
                    if let Some(entered) = entered.take() {
                        entered.send(()).expect("entered wait");
                    }
                    if stopping.load(Ordering::Acquire) {
                        Err("pool stopped".into())
                    } else {
                        Ok(())
                    }
                })
                .map(|mut guard| *guard += 1);
                send.send(result).expect("wait result");
            });
            started
                .recv_timeout(Duration::from_secs(1))
                .expect("waiting");
            if closed {
                drop(peer);
            } else {
                stop.store(true, Ordering::Release);
            }
            let error = result
                .recv_timeout(Duration::from_secs(1))
                .expect("cancelled with lock still held")
                .expect_err("no admission");
            assert_eq!(error, if closed { DISCONNECTED } else { "pool stopped" });
            worker.join().expect("joined waiter");
            assert_eq!(*held, 41);
            drop(held);
            assert_eq!(
                *lock(&mutex, &crate::cancelling::capture(), || Ok(())).expect("recovery"),
                41
            );
        }
    }

    #[test]
    fn disconnected_call_cannot_acquire_an_available_pool() {
        let (ours, peer) = UnixStream::pair().expect("peer");
        let caller = crate::cancelling::with_peer(&ours, crate::cancelling::capture);
        peer.shutdown(std::net::Shutdown::Both).expect("full close");
        drop(peer);
        let mutex = Mutex::new(0);
        assert_eq!(
            lock(&mutex, &caller, || Ok(())).expect_err("refused"),
            DISCONNECTED
        );
    }

    #[test]
    fn full_peer_shutdown_refuses_pool_even_with_duplicated_descriptor() {
        let (ours, peer) = UnixStream::pair().expect("peer");
        let inherited = peer.try_clone().expect("duplicate peer");
        let caller = crate::cancelling::with_peer(&ours, crate::cancelling::capture);
        drop(peer);
        let mutex = Mutex::new(17);
        assert_eq!(
            *lock(&mutex, &caller, || Ok(())).expect("peer still open"),
            17
        );
        inherited
            .shutdown(std::net::Shutdown::Both)
            .expect("full close");
        assert_eq!(
            lock(&mutex, &caller, || Ok(())).expect_err("refused"),
            DISCONNECTED
        );
        assert_eq!(*mutex.lock().expect("unchanged pool"), 17);
    }
}
