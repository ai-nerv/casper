use crate::scratch::Scratch;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

/// An isolated Chromium profile and its owned process group.
pub struct Managed {
    child: Child,
    profile: Scratch,
    endpoint: String,
}

impl Managed {
    pub fn launch(program: &str, no_sandbox: bool) -> Result<Self, String> {
        let jail = crate::jail::Jail::from_env();
        if jail.on() {
            return Err("managed Chromium cannot start inside CASPER_JAIL; launch a dedicated browser separately and grant reach to browse calls".into());
        }
        let temporary = std::env::temp_dir();
        let root = if temporary.as_os_str().as_encoded_bytes().len() <= 30 {
            temporary.as_path()
        } else {
            std::path::Path::new("/tmp")
        };
        let profile = Scratch::in_dir(root, "cb", "profile");
        let mut args = vec![
            "--headless=new".into(),
            "--remote-debugging-address=127.0.0.1".into(),
            "--remote-debugging-port=0".into(),
            format!("--user-data-dir={}", profile.display()),
            "--no-first-run".into(),
            "--no-default-browser-check".into(),
            "--disable-background-networking".into(),
            "--disable-component-update".into(),
            "--disable-sync".into(),
            "--disable-extensions".into(),
            "--disable-gpu".into(),
            "--window-size=1024,768".into(),
            "about:blank".into(),
        ];
        if no_sandbox {
            args.push("--no-sandbox".into());
        }
        let mut prepared = jail
            .prepare(program, &args, None, &[], false)
            .map_err(|why| why.to_string())?;
        let child = prepared
            .command()
            .env("TMPDIR", &*profile)
            .env("XDG_CACHE_HOME", profile.join("cache"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|why| format!("Chromium could not start: {why}"))?;
        let mut browser = Self {
            child,
            profile,
            endpoint: String::new(),
        };
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if let Some(status) = browser.child.try_wait().map_err(|why| why.to_string())? {
                return Err(format!(
                    "Chromium exited during startup: {status}; check its stderr and sandbox support"
                ));
            }
            if let Ok(port) = std::fs::read_to_string(browser.profile.join("DevToolsActivePort")) {
                let mut lines = port.lines();
                let port: u16 = lines
                    .next()
                    .ok_or("Chromium returned no debug port")?
                    .parse()
                    .map_err(|_| "invalid Chromium debug port")?;
                browser.endpoint = format!("http://127.0.0.1:{port}/");
                let configuration =
                    serde_json::json!({"endpoint":browser.endpoint,"timeout_secs":1});
                let endpoint = super::Endpoint::parse(&browser.endpoint, Some(&configuration))?;
                if let Ok(version) = endpoint.request("/json/version", false) {
                    let expected = format!(
                        "ws://127.0.0.1:{port}{}",
                        lines
                            .next()
                            .ok_or("Chromium returned no browser debugger")?
                    );
                    if version["webSocketDebuggerUrl"] == expected {
                        return Ok(browser);
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        Err("Chromium startup timed out".into())
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn profile(&self) -> &std::path::Path {
        &self.profile
    }
}

impl Drop for Managed {
    fn drop(&mut self) {
        let group = rustix::process::Pid::from_raw(self.child.id() as i32);
        if let Some(group) = group {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::TERM);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if self.child.try_wait().is_ok_and(|status| status.is_some()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if let Some(group) = group {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub enum Owned {
    Local(Managed),
    Supervised(super::supervisor::Supervisor),
}

impl Owned {
    pub fn endpoint(&self) -> &str {
        match self {
            Self::Local(browser) => browser.endpoint(),
            Self::Supervised(browser) => browser.endpoint(),
        }
    }
    pub fn readiness(&self) -> serde_json::Value {
        match self {
            Self::Local(browser) => {
                serde_json::json!({"endpoint":browser.endpoint(),"pid":browser.pid(),"profile":browser.profile()})
            }
            Self::Supervised(browser) => serde_json::json!({"endpoint":browser.endpoint()}),
        }
    }
}

pub fn from_args(args: &[String]) -> Result<Option<Owned>, String> {
    let verb = args.first().map(String::as_str);
    let flag = if verb == Some("browser") {
        "--program"
    } else {
        "--browser"
    };
    let program = crate::mcp::option(args, flag);
    if program.is_none() && verb != Some("browser") {
        return Ok(None);
    }
    let program = program.as_deref().unwrap_or("chromium");
    let no_sandbox = args.iter().any(|arg| arg == "--no-sandbox");
    if verb == Some("browser") {
        Managed::launch(program, no_sandbox)
            .map(Owned::Local)
            .map(Some)
    } else {
        super::supervisor::Supervisor::launch(program, no_sandbox)
            .map(Owned::Supervised)
            .map(Some)
    }
}

pub async fn wait() -> Result<(), String> {
    use tokio::io::AsyncReadExt;
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|why| why.to_string())?;
    let mut stdin = tokio::io::stdin();
    let mut buffer = [0; 512];
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return Ok(()),
            _ = term.recv() => return Ok(()),
            read = stdin.read(&mut buffer) => if read.map_err(|why| why.to_string())? == 0 { return Ok(()); },
        }
    }
}
