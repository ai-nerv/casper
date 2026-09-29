use std::io::{BufRead, BufReader, Read};
use std::process::{Child, ChildStdin, Stdio};
use std::time::{Duration, Instant};

pub struct Supervisor {
    child: Child,
    input: Option<ChildStdin>,
    endpoint: String,
}

impl Supervisor {
    pub fn launch(program: &str, no_sandbox: bool) -> Result<Self, String> {
        let root = std::env::current_dir().map_err(|why| why.to_string())?;
        let mut command = crate::jail::worker_std(&root).map_err(|why| why.to_string())?;
        command.args(["browser", "--program", program]);
        if no_sandbox {
            command.arg("--no-sandbox");
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|why| format!("browser supervisor could not start: {why}"))?;
        let input = child.stdin.take();
        let pipe = child
            .stdout
            .take()
            .ok_or("browser supervisor has no readiness pipe")?;
        let mut owned = Self {
            child,
            input,
            endpoint: String::new(),
        };
        let (send, receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let ready = BufReader::new(pipe.take(8192)).lines().next();
            let _ = send.send(ready);
        });
        let reply = receive
            .recv_timeout(Duration::from_secs(20))
            .map_err(|_| "browser supervisor startup timed out")?
            .ok_or("browser supervisor exited without readiness")?
            .map_err(|why| why.to_string())?;
        let reply: serde_json::Value =
            serde_json::from_str(&reply).map_err(|why| why.to_string())?;
        owned.endpoint = reply["endpoint"]
            .as_str()
            .ok_or("browser supervisor returned no endpoint")?
            .into();
        Ok(owned)
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        drop(self.input.take());
        let deadline = Instant::now() + Duration::from_secs(4);
        while Instant::now() < deadline {
            if self.child.try_wait().is_ok_and(|status| status.is_some()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if let Some(pid) = rustix::process::Pid::from_raw(self.child.id() as i32) {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if self.child.try_wait().is_ok_and(|status| status.is_some()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
