use super::{
    Config, Error, Snapshot, Store, cancellation::Operation, control::Control, discover, token,
};
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub(super) fn now() -> Result<u64, Error> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| time.as_secs())
        .map_err(|_| Error::Credential)
}

pub(super) async fn checked_snapshot(
    store: &Arc<Store>,
    operation: &Operation,
) -> Result<Snapshot, Error> {
    let store = Arc::clone(store);
    let cancelled = Arc::clone(&operation.0);
    tokio::task::spawn_blocking(move || store.snapshot_cancellable(&cancelled))
        .await
        .map_err(|_| Error::Store)?
}

fn bound(state: &Snapshot, epoch: u64) -> Result<(), Error> {
    if state.epoch != epoch {
        return Err(Error::Changed);
    }
    if state.credential.is_none() {
        return Err(Error::Required);
    }
    Ok(())
}

pub(super) async fn current(
    store: &Arc<Store>,
    config: &Config,
    endpoint: &str,
    epoch: u64,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
) -> Result<Snapshot, Error> {
    let operation = Operation::new(
        Duration::from_secs(config.timeout_secs.min(30)),
        Some(cancelled),
    );
    operation.0.check()?;
    tokio::time::timeout_at(operation.0.deadline().into(), async {
        let state = checked_snapshot(store, &operation).await?;
        bound(&state, epoch)?;
        let time = now()?;
        if state
            .credential
            .as_ref()
            .is_some_and(|credential| credential.fresh(time))
        {
            return Ok(state);
        }
        if !crate::jail::Jail::from_env().allows_network() {
            return Err(Error::Network);
        }
        let lease = loop {
            operation.0.check()?;
            let held = Arc::clone(store);
            if let Some(lease) = tokio::task::spawn_blocking(move || held.try_refresh())
                .await
                .map_err(|_| Error::Store)??
            {
                break lease;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        let state = checked_snapshot(store, &operation).await?;
        bound(&state, epoch)?;
        let previous = state.credential.as_ref().ok_or(Error::Required)?;
        let time = now()?;
        if previous.fresh(time) {
            return Ok(state);
        }
        let refresh = previous.refresh_token.clone().ok_or(Error::Required)?;
        let held = Arc::clone(store);
        let cancelled = Arc::clone(&operation.0);
        let (state, lease) = tokio::task::spawn_blocking(move || {
            held.begin_refresh(&state, &lease, time, &cancelled)
                .map(|state| (state, lease))
        })
        .await
        .map_err(|_| Error::Store)??;
        let discovery = discover(config, endpoint).await?;
        let form = vec![
            ("grant_type".into(), "refresh_token".into()),
            ("refresh_token".into(), refresh),
            ("client_id".into(), config.client_id.clone()),
            ("resource".into(), config.bind(endpoint)?.resource().into()),
        ];
        let response = Control::new(config, endpoint)?
            .request(&discovery.token_endpoint, Some(&form), false)
            .await?;
        if !response.status.is_success() {
            return Err(Error::Required);
        }
        let previous = state.credential.as_ref().ok_or(Error::Required)?;
        let credential =
            token::credential(&response.body, Some(previous), &previous.scopes, now()?)?;
        let held = Arc::clone(store);
        let cancelled = Arc::clone(&operation.0);
        tokio::task::spawn_blocking(move || held.rotate(&state, credential, &lease, &cancelled))
            .await
            .map_err(|_| Error::Store)??;
        let state = checked_snapshot(store, &operation).await?;
        bound(&state, epoch)?;
        Ok(state)
    })
    .await
    .map_err(|_| Error::Timeout)?
}

#[cfg(test)]
mod tests;
