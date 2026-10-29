use super::*;

impl Hub {
    pub(super) fn admit<T>(
        &mut self,
        caller: &crate::cancelling::Captured,
        scope: &lifecycle::Scope,
        operation: impl FnOnce(&mut Self) -> Result<T, String>,
    ) -> Result<T, String> {
        cancellation::check(caller)?;
        scope.check()?;
        let previous: BTreeMap<_, _> = self
            .sessions
            .iter()
            .map(|(name, session)| (name.clone(), Arc::downgrade(session)))
            .collect();
        let result = operation(self);
        if let Err(error) = cancellation::check(caller).and_then(|()| scope.check()) {
            let fresh: Vec<_> = self
                .sessions
                .iter()
                .filter(|(name, session)| {
                    previous
                        .get(*name)
                        .is_none_or(|old| old.as_ptr() != Arc::as_ptr(session))
                })
                .map(|(name, _)| name.clone())
                .collect();
            for name in fresh {
                if let Some(session) = self.sessions.remove(&name) {
                    session.stop();
                    close(&self.runtime, session);
                }
            }
            return Err(error);
        }
        result
    }
}

#[cfg(test)]
mod tests;
