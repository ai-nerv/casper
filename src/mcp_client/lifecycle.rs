use std::{
    future::Future,
    sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError},
};

pub(super) struct Scope(tokio::sync::watch::Sender<bool>);

impl Scope {
    fn new() -> Self {
        Self(tokio::sync::watch::channel(false).0)
    }

    pub(super) fn check(&self) -> Result<(), String> {
        if *self.0.borrow() {
            Err("MCP pool stopping; remote effects may have occurred; not retried".into())
        } else {
            Ok(())
        }
    }

    pub(super) async fn run<T>(
        &self,
        work: impl Future<Output = Result<T, String>>,
    ) -> Result<T, String> {
        let mut changed = self.0.subscribe();
        self.check()?;
        tokio::select! {
            biased;
            _ = changed.changed() => Err("MCP pool stopping; remote effects may have occurred; not retried".into()),
            result = work => { self.check()?; result }
        }
    }
}

struct Gate {
    closing: bool,
    scope: Arc<Scope>,
}

static GATE: OnceLock<Mutex<Gate>> = OnceLock::new();
static SERIAL: Mutex<()> = Mutex::new(());

fn gate() -> MutexGuard<'static, Gate> {
    GATE.get_or_init(|| {
        Mutex::new(Gate {
            closing: false,
            scope: Arc::new(Scope::new()),
        })
    })
    .lock()
    .unwrap_or_else(PoisonError::into_inner)
}

pub(super) fn enter() -> Result<Arc<Scope>, String> {
    let gate = gate();
    if gate.closing {
        return Err("MCP pool stopping; request not admitted".into());
    }
    Ok(gate.scope.clone())
}

pub(super) struct Closing {
    _serial: MutexGuard<'static, ()>,
}

pub(super) fn closing() -> Closing {
    let serial = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
    let mut gate = gate();
    gate.closing = true;
    gate.scope.0.send_replace(true);
    Closing { _serial: serial }
}

impl Drop for Closing {
    fn drop(&mut self) {
        let mut gate = gate();
        gate.scope = Arc::new(Scope::new());
        gate.closing = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancellation_wakes_pending_work_and_refuses_ready_work() {
        let scope = Arc::new(Scope::new());
        let waiting = scope.clone();
        let (ready, started) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            waiting
                .run(async {
                    ready.send(()).expect("ready");
                    std::future::pending::<Result<(), String>>().await
                })
                .await
        });
        started.await.expect("admitted work");
        scope.0.send_replace(true);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(500), task)
                .await
                .expect("woken")
                .expect("task")
                .is_err()
        );
        assert!(scope.run(async { Ok(()) }).await.is_err());
        let finishing = Scope::new();
        assert!(
            finishing
                .run(async {
                    finishing.0.send_replace(true);
                    Ok(())
                })
                .await
                .is_err()
        );
        assert!(Scope::new().run(async { Ok(()) }).await.is_ok());
    }
}
