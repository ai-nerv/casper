use super::{
    Config, Error, Store, callback::Callback, cancellation::Operation, control::Control, discover,
    pkce::Proof, token,
};
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Perform an explicit manual-browser login and commit only to the starting store generation.
pub async fn login<F: std::future::Future<Output = Result<(), Error>>>(
    config: &Config,
    endpoint: &str,
    display: impl FnOnce(String) -> F,
) -> Result<u64, Error> {
    let binding = config.bind(endpoint)?;
    let operation = Operation::new(Duration::from_secs(config.timeout_secs), None);
    if !crate::jail::Jail::from_env().allows_network() {
        return Err(Error::Network);
    }
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
        let held = Arc::clone(&store);
        let cancelled = Arc::clone(&operation.0);
        let snapshot = tokio::task::spawn_blocking(move || held.snapshot_cancellable(&cancelled))
            .await
            .map_err(|_| Error::Store)??;
        let mut discovery = discover(config, endpoint).await?;
        if let Some(previous) = &snapshot.credential {
            for scope in &previous.scopes {
                if !discovery.scopes.contains(scope) {
                    discovery.scopes.push(scope.clone());
                }
            }
            if discovery.scopes.len() > 32 {
                return Err(Error::Configuration);
            }
        }
        let callback = Callback::bind(config.callback_port).await?;
        let redirect = callback.redirect.clone();
        let proof = Proof::new()?;
        let authorization = proof.authorization(config, &discovery, &redirect)?;
        display(authorization.into()).await?;
        let code = callback.receive(&proof.state, &discovery).await?;
        let form = vec![
            ("grant_type".into(), "authorization_code".into()),
            ("client_id".into(), config.client_id.clone()),
            ("redirect_uri".into(), redirect),
            ("resource".into(), config.bind(endpoint)?.resource().into()),
            ("code".into(), code),
            ("code_verifier".into(), proof.verifier),
        ];
        let response = Control::new(config, endpoint)?
            .request(&discovery.token_endpoint, Some(&form), false)
            .await?;
        if !response.status.is_success() {
            return Err(Error::Credential);
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::Credential)?
            .as_secs();
        let credential = token::credential(
            &response.body,
            snapshot.credential.as_ref(),
            &discovery.scopes,
            now,
        )?;
        let cancelled = Arc::clone(&operation.0);
        tokio::task::spawn_blocking(move || {
            store.replace_cancellable(snapshot.generation, credential, &cancelled)
        })
        .await
        .map_err(|_| Error::Store)?
    })
    .await
    .map_err(|_| Error::Timeout)?
}
