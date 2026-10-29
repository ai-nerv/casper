use super::*;

impl Hub {
    pub(super) fn new() -> Result<Self, String> {
        let scope = lifecycle::enter()?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|why| format!("MCP runtime: {why}"))?;
        Ok(Self {
            runtime,
            scope,
            sessions: BTreeMap::new(),
        })
    }

    pub(super) fn session(&mut self, name: &str, server: &Server) -> Result<Arc<Session>, String> {
        let caller = crate::cancelling::capture();
        cancellation::check(&caller)?;
        self.scope.check()?;
        if (server.url.is_some() || server.network || server.needs == "reach")
            && !crate::jail::Jail::from_env().allows_network()
        {
            observing::Trace::new().finish(crate::events::ServerOutcome::Refused);
            if let Some(old) = self.sessions.remove(name) {
                close(&self.runtime, old);
            }
            return Err("MCP connection requires a network grant".into());
        }
        let auth = server
            .auth
            .as_ref()
            .map(|config| {
                self.runtime.block_on(cancellation::run(
                    &caller,
                    self.scope.run(async {
                        oauth::Access::open(
                            config,
                            server
                                .url
                                .as_deref()
                                .ok_or_else(|| oauth::Error::Configuration.to_string())?,
                        )
                        .await
                        .map_err(|error| error.to_string())
                    }),
                ))
            })
            .transpose();
        let auth = match auth {
            Ok(auth) => auth,
            Err(error) => {
                observing::Trace::new().finish(crate::events::ServerOutcome::Refused);
                if let Some(old) = self.sessions.remove(name) {
                    close(&self.runtime, old);
                }
                return Err(error.to_string());
            }
        };
        let stamp = format!(
            "{}:{:?}:{:?}:{:?}",
            serde_json::to_string(server).map_err(|why| why.to_string())?,
            std::env::current_dir(),
            std::env::var_os(crate::jail::WANTED),
            auth.as_ref().map(|auth| auth.epoch()),
        );
        if let Some(session) = self.sessions.get(name)
            && session.stamp == stamp
            && !session.service.is_closed()
            && !session.stopped.load(Ordering::Acquire)
            && !session.over.load(Ordering::Relaxed)
            && !session.auth.as_ref().is_some_and(|auth| auth.stopped())
        {
            return Ok(Arc::clone(session));
        }
        if let Some(old) = self.sessions.remove(name) {
            close(&self.runtime, old);
        }
        let mut trace = observing::Trace::new();
        let mut session = self
            .runtime
            .block_on(cancellation::run(
                &caller,
                self.scope.run(async {
                    let jail = crate::jail::Jail::from_env();
                    if (server.url.is_some() || server.network || server.needs == "reach")
                        && !jail.allows_network()
                    {
                        trace.finish(crate::events::ServerOutcome::Refused);
                        return Err("MCP connection requires a network grant".to_owned());
                    }
                    let timeout = Duration::from_secs(server.timeout_secs);
                    let over = Arc::new(AtomicBool::new(false));
                    let mut held = None;
                    let connected = if let Some(command) = &server.command {
                        // Forked from a runtime worker: a child's death signal follows the thread that
                        // forked it, and a `serve` connection's thread ends with its call.
                        let (command, args, env) =
                            (command.clone(), server.args.clone(), server.env.clone());
                        let flag = Arc::clone(&over);
                        let mut launch = tokio::task::JoinSet::new();
                        launch.spawn(
                            async move { stdio::launch(&jail, &command, &args, &env, flag) },
                        );
                        let launched = launch
                            .join_next()
                            .await
                            .ok_or("MCP stdio launch cancelled")?
                            .map_err(|why| format!("MCP stdio: {why}"))??;
                        held = Some(launched.server);
                        let transport = (launched.read, launched.write);
                        tokio::time::timeout(timeout, Client.serve(transport)).await
                    } else {
                        let url = server.url.as_deref().ok_or("MCP URL missing")?;
                        let client = if let Some(auth) = &server.auth {
                            oauth::client(auth, url)
                                .await
                                .map_err(|error| error.to_string())?
                                .1
                        } else {
                            reqwest::Client::builder()
                                .no_proxy()
                                .no_gzip()
                                .redirect(reqwest::redirect::Policy::none())
                                .connect_timeout(Duration::from_secs(10))
                                .build()
                                .map_err(|why| format!("MCP HTTP client: {why}"))?
                        };
                        let mut config = StreamableHttpClientTransportConfig::with_uri(url)
                            .max_concurrent_requests(8)
                            .control_request_timeout(timeout);
                        config.max_sse_event_size = REPLY;
                        config.reinit_on_expired_session = false;
                        config.retry_config = Arc::new(
                            http::Patience::new(Arc::clone(&over)).with_auth(auth.clone()),
                        );
                        for (key, value) in &server.headers {
                            config.custom_headers.insert(
                                key.parse()
                                    .map_err(|why| format!("MCP header name: {why}"))?,
                                value
                                    .parse()
                                    .map_err(|why| format!("MCP header value: {why}"))?,
                            );
                        }
                        let client = http::Bounded::new(client, REPLY, Arc::clone(&over))
                            .with_auth(auth.clone())
                            .map_err(|error| error.to_string())?;
                        tokio::time::timeout(
                            timeout,
                            Client
                                .serve(StreamableHttpClientTransport::with_client(client, config)),
                        )
                        .await
                    };
                    let service = connected
                        .map_err(|_| "MCP initialization timed out".to_owned())?
                        .map_err(|why| {
                            oversized(&over).unwrap_or(format!("MCP initialization: {why}"))
                        })?;
                    let mut session = Session {
                        stamp,
                        auth: auth.clone(),
                        auth_key: server
                            .auth
                            .as_ref()
                            .and_then(|config| config.bind(server.url.as_deref()?).ok())
                            .map(|binding| binding.key()),
                        service,
                        cards: Vec::new(),
                        catalog: BTreeMap::new(),
                        over,
                        server: Mutex::new(held),
                        stopped: AtomicBool::new(false),
                        root: std::env::current_dir()
                            .map_err(|e| e.to_string())?
                            .canonicalize()
                            .map_err(|e| e.to_string())?,
                        sequence: std::sync::atomic::AtomicU64::new(0),
                        read_slots: tokio::sync::Semaphore::new(8),
                        trace: observing::Trace::default(),
                    };
                    let discovered =
                        tokio::time::timeout(timeout, session.discover(name, server)).await;
                    match discovered {
                        Ok(Ok(())) => Ok(session),
                        failure => {
                            let _ = session.service.cancel().await;
                            Err(oversized(&session.over).unwrap_or(match failure {
                                Ok(Err(why)) => why,
                                _ => "MCP discovery timed out".into(),
                            }))
                        }
                    }
                }),
            ))
            .map_err(|why| {
                trace.finish(crate::events::ServerOutcome::Failed);
                format!("MCP server {name}: {why}")
            })?;
        cancellation::check(&caller)?;
        trace.connected();
        session.trace = trace;
        let session = Arc::new(session);
        self.sessions.insert(name.to_owned(), Arc::clone(&session));
        Ok(session)
    }
}
