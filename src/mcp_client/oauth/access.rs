use super::{Config, Error, Store, cancellation::Operation, refresh};
use std::sync::{
    Arc, Mutex, PoisonError, Weak,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

static ACTIVE: Mutex<Vec<Weak<Access>>> = Mutex::new(Vec::new());

pub(in crate::mcp_client) struct Access {
    store: Arc<Store>,
    epoch: u64,
    endpoint: url::Url,
    config: Config,
    halted: Arc<AtomicBool>,
    changed: tokio::sync::watch::Sender<bool>,
    key: String,
    monitor: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Drop for Access {
    fn drop(&mut self) {
        if let Some(task) = self
            .monitor
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            task.abort();
        }
    }
}

impl Access {
    pub(in crate::mcp_client) async fn open(
        config: &Config,
        endpoint: &str,
    ) -> Result<Arc<Self>, Error> {
        let binding = config.bind(endpoint)?;
        let operation = Operation::new(Duration::from_secs(config.timeout_secs.min(30)), None);
        tokio::time::timeout_at(operation.0.deadline().into(), async {
            let cancelled = Arc::clone(&operation.0);
            let store = Arc::new(
                tokio::task::spawn_blocking(move || {
                    cancelled.check()?;
                    let store = Store::open(binding)?;
                    cancelled.check()?;
                    Ok::<_, Error>(store)
                })
                .await
                .map_err(|_| Error::Store)??,
            );
            Self::attach_inner(config, endpoint, store, &operation).await
        })
        .await
        .map_err(|_| Error::Timeout)?
    }

    #[cfg(test)]
    pub(in crate::mcp_client) async fn attach(
        config: &Config,
        endpoint: &str,
        store: Arc<Store>,
    ) -> Result<Arc<Self>, Error> {
        let operation = Operation::new(Duration::from_secs(config.timeout_secs.min(30)), None);
        tokio::time::timeout_at(
            operation.0.deadline().into(),
            Self::attach_inner(config, endpoint, store, &operation),
        )
        .await
        .map_err(|_| Error::Timeout)?
    }

    async fn attach_inner(
        config: &Config,
        endpoint: &str,
        store: Arc<Store>,
        operation: &Operation,
    ) -> Result<Arc<Self>, Error> {
        let binding = config.bind(endpoint)?;
        if !store.matches(&binding) {
            return Err(Error::Configuration);
        }
        let key = binding.key();
        let snapshot = refresh::checked_snapshot(&store, operation).await?;
        let access = Arc::new(Self {
            store,
            epoch: snapshot.epoch,
            endpoint: url::Url::parse(endpoint).map_err(|_| Error::Configuration)?,
            config: config.clone(),
            halted: Arc::new(AtomicBool::new(false)),
            changed: tokio::sync::watch::channel(false).0,
            key,
            monitor: Mutex::new(None),
        });
        {
            let mut active = ACTIVE.lock().unwrap_or_else(PoisonError::into_inner);
            active.retain(|access| access.strong_count() != 0);
            if active.len() >= 256 {
                return Err(Error::Busy);
            }
            active.push(Arc::downgrade(&access));
        }
        let weak = Arc::downgrade(&access);
        let task = tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                let Some(access) = weak.upgrade() else {
                    break;
                };
                if access.stopped() {
                    break;
                }
                let (store, epoch) = (Arc::clone(&access.store), access.epoch);
                let operation = Operation::new(Duration::from_secs(2), Some(access.halted.clone()));
                drop(access);
                let invalid = refresh::checked_snapshot(&store, &operation)
                    .await
                    .map_or(true, |state| {
                        state.epoch != epoch || state.credential.is_none()
                    });
                let Some(access) = weak.upgrade() else {
                    break;
                };
                if invalid {
                    access.stop();
                    break;
                }
            }
        });
        *access
            .monitor
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(task);
        access.token().await?;
        Ok(access)
    }

    pub(in crate::mcp_client) fn epoch(&self) -> u64 {
        self.epoch
    }

    pub(in crate::mcp_client) fn stopped(&self) -> bool {
        self.halted.load(Ordering::Acquire)
    }
    pub(in crate::mcp_client) fn stop(&self) {
        self.halted.store(true, Ordering::Release);
        self.changed.send_replace(true);
    }

    pub(in crate::mcp_client) fn stop_key(key: &str) {
        let mut active = ACTIVE.lock().unwrap_or_else(PoisonError::into_inner);
        active.retain(|entry| {
            let Some(access) = entry.upgrade() else {
                return false;
            };
            if access.key == key {
                access.stop();
            }
            true
        });
    }

    pub(in crate::mcp_client) async fn cancelled(&self) {
        let mut changed = self.changed.subscribe();
        if self.stopped() {
            return;
        }
        let _ = changed.changed().await;
    }

    pub(in crate::mcp_client) fn destination(&self, endpoint: &str) -> Result<(), Error> {
        if url::Url::parse(endpoint).ok().as_ref() != Some(&self.endpoint) {
            return Err(Error::Configuration);
        }
        Ok(())
    }

    pub(in crate::mcp_client) async fn token(&self) -> Result<String, Error> {
        let mut changed = self.changed.subscribe();
        if self.stopped() {
            return Err(Error::Required);
        }
        let renewed = tokio::select! {
            biased;
            _ = changed.changed() => Err(Error::Cancelled),
            result = refresh::current(&self.store, &self.config, self.endpoint.as_str(), self.epoch, Arc::clone(&self.halted)) => result,
        };
        let result = renewed
            .and_then(|state| state.credential.ok_or(Error::Required))
            .and_then(|credential| {
                let now = refresh::now()?;
                if credential.expires_at.is_some_and(|expires| expires <= now) {
                    return Err(Error::Required);
                }
                Ok(credential.access_token)
            });
        if result.is_err() {
            self.stop();
        }
        result
    }
}

#[cfg(test)]
mod contention;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn discarded_auth_handles_release_monitors_registry_slots_and_store_descriptors() {
        let root = crate::scratch::Scratch::new("oauth", "access-lifecycle");
        let config: Config = serde_json::from_str(r#"{"issuer":"https://issuer.example","resource":"https://resource.example/mcp","client_id":"native-lifecycle"}"#).expect("config");
        let endpoint = config.resource.as_str();
        let store = Arc::new(
            Store::at(&root.join("store"), config.bind(endpoint).expect("binding")).expect("store"),
        );
        store
            .replace(
                0,
                super::super::Credential {
                    access_token: "access-secret-canary".into(),
                    refresh_token: None,
                    expires_at: None,
                    scopes: vec![],
                },
            )
            .expect("seed");
        let owned = || {
            std::fs::read_dir("/proc/self/fd")
                .expect("descriptors")
                .filter_map(Result::ok)
                .filter_map(|entry| std::fs::read_link(entry.path()).ok())
                .filter(|path| path.starts_with(&*root))
                .count()
        };
        assert_eq!(owned(), 1);
        for _ in 0..260 {
            let access = Access::attach(&config, endpoint, store.clone())
                .await
                .expect("slot reused");
            let weak = Arc::downgrade(&access);
            let monitor = access
                .monitor
                .lock()
                .expect("monitor")
                .as_ref()
                .expect("task")
                .abort_handle();
            drop(access);
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while weak.upgrade().is_some() || !monitor.is_finished() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("owned monitor terminates");
        }
        store.logout().expect("logout");
        assert!(matches!(
            Access::attach(&config, endpoint, store.clone()).await,
            Err(Error::Required)
        ));
        let weak = Arc::downgrade(&store);
        drop(store);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while weak.upgrade().is_some() || owned() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("all owner descriptors released");
    }

    #[tokio::test]
    async fn registry_cancellation_is_key_scoped_and_wakes_existing_waiters() {
        let root = crate::scratch::Scratch::new("oauth", "access-registry");
        let config: Config = serde_json::from_str(r#"{"issuer":"https://issuer.example","resource":"https://resource.example/mcp","client_id":"native-registry"}"#).expect("config");
        let endpoint = "https://resource.example/mcp";
        let binding = config.bind(endpoint).expect("binding");
        let key = binding.key();
        let store = Arc::new(Store::at(&root.join("store"), binding).expect("store"));
        let credential = super::super::Credential {
            access_token: "access-secret-canary".into(),
            refresh_token: None,
            expires_at: None,
            scopes: vec![],
        };
        store.replace(0, credential.clone()).expect("seed");
        let first = Access::attach(&config, endpoint, store.clone())
            .await
            .expect("first");
        let second = Access::attach(&config, endpoint, store)
            .await
            .expect("second");
        let mut other_config = config.clone();
        other_config.client_id = "unrelated-client".into();
        let other = Arc::new(
            Store::at(
                &root.join("store"),
                other_config.bind(endpoint).expect("binding"),
            )
            .expect("other"),
        );
        other.replace(0, credential).expect("seed other");
        let other = Access::attach(&other_config, endpoint, other)
            .await
            .expect("other access");
        let waiting = first.clone();
        let waiter = tokio::spawn(async move { waiting.cancelled().await });
        tokio::task::yield_now().await;
        Access::stop_key(&key);
        tokio::time::timeout(std::time::Duration::from_millis(500), waiter)
            .await
            .expect("wake")
            .expect("task");
        assert!(first.stopped() && second.stopped());
        assert!(!other.stopped());
        assert_eq!(
            other.token().await.expect("unrelated token"),
            "access-secret-canary"
        );
        assert_eq!(first.token().await.expect_err("stopped"), Error::Required);
    }
}
