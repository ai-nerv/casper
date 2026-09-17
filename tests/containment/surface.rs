use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::path::Path;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

struct Surface {
    child: super::lifecycle::Owned,
    input: ChildStdin,
    output: Receiver<Value>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Surface {
    fn open() -> Self {
        let mut child = super::lifecycle::Owned::new(
            Command::new(env!("CARGO_BIN_EXE_casper"))
                .args(["surface", "accept"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .expect("actual surface"),
        );
        let input = child.0.stdin.take().expect("input");
        let output = child.0.stdout.take().expect("output");
        let (send, receive) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in std::io::BufReader::new(output).lines() {
                let Ok(line) = line else { break };
                let frame: Value = serde_json::from_str(&line).expect("surface frame");
                if send.send(frame).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            input,
            output: receive,
            reader: Some(reader),
        }
    }

    fn send(&mut self, frame: Value) {
        writeln!(self.input, "{frame}").expect("send frame");
        self.input.flush().expect("flush frame");
    }

    fn type_line(&mut self, line: &str) {
        for key in line.chars().map(|c| c.to_string()).chain(["enter".into()]) {
            self.send(json!({"event":"key", "key":key}));
        }
    }

    fn until(&mut self, accepts: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self.output.recv_timeout(Duration::from_millis(10)) {
                Ok(frame) => {
                    assert!(
                        !rows(&frame).join("\n").contains("no job control"),
                        "{frame}"
                    );
                    if accepts(&frame) {
                        return frame;
                    }
                    assert_ne!(frame["event"], "done", "surface ended early: {frame}");
                }
                Err(RecvTimeoutError::Timeout) => self.send(json!({"event":"tick"})),
                Err(RecvTimeoutError::Disconnected) => panic!("surface disconnected"),
            }
            assert!(
                Instant::now() < deadline,
                "surface did not reach expected state"
            );
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        self.child.stop();
        if let Some(reader) = self.reader.take() {
            reader.join().expect("frame reader");
        }
    }
}

fn rows(frame: &Value) -> Vec<String> {
    frame["lines"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|line| {
            line.as_array()
                .expect("spans")
                .iter()
                .map(|span| span["text"].as_str().expect("text"))
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

pub(super) fn review(root: &Path) {
    let config = root.join("config/casper");
    std::fs::create_dir_all(&config).expect("surface config");
    std::fs::write(
        config.join("tools.lua"),
        r#"
casper.tool("accept", {
  description = "Synthetic interactive acceptance",
  parameters = {type = "object", properties = {}},
  screen = function()
    return {command = "/bin/bash", args = {"--noprofile", "--norc", "-c",
      "export PS1='CAPTURE> '; exec /bin/bash --noprofile --norc -i"}}
  end,
})
"#,
    )
    .expect("synthetic declaration");
    let mut surface = Surface::open();
    surface.send(json!({"event":"open", "rows":6, "cols":60, "args":{}}));
    let opened = surface.until(|frame| rows(frame).iter().any(|line| line == "CAPTURE>"));
    assert_eq!(rows(&opened).len(), 6);
    assert!(!opened["cursor"].is_null());
    surface.type_line("printf 'INPUT_OK\\n'");
    let typed = surface.until(|frame| rows(frame).iter().any(|line| line == "INPUT_OK"));
    surface.send(json!({"event":"resize", "rows":9, "cols":50}));
    surface.type_line("/bin/stty size");
    let resized = surface.until(|frame| rows(frame).iter().any(|line| line == "9 50"));
    assert_eq!(rows(&resized).len(), 9);
    assert!(rows(&resized).iter().all(|line| line.chars().count() <= 50));
    surface.type_line("/bin/sh -c 'echo ready > waiting; exec /bin/sleep 30'");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !root.join("work/waiting").exists() {
        assert!(Instant::now() < deadline, "foreground job never started");
        std::thread::sleep(Duration::from_millis(10));
    }
    surface.send(json!({"event":"key", "key":"ctrl+c"}));
    let interrupted = surface.until(|frame| {
        let painted = rows(frame);
        painted.iter().any(|line| line.contains("^C"))
            && painted
                .iter()
                .rev()
                .find(|line| !line.is_empty())
                .is_some_and(|line| line == "CAPTURE>")
    });
    surface.type_line("exit 0");
    let exited = surface.until(|frame| frame["event"] == "done");
    assert!(
        exited["answered"]
            .as_str()
            .is_some_and(|text| text.contains("status 0")),
        "{exited}"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = surface.child.0.try_wait().expect("surface exit") {
            assert!(status.success(), "surface exited with {status}");
            break;
        }
        assert!(Instant::now() < deadline, "surface process failed to exit");
        std::thread::sleep(Duration::from_millis(10));
    }
    let report =
        std::path::PathBuf::from(std::env::var_os("CASPER_SURFACE_REPORT").expect("report path"));
    std::fs::create_dir_all(report.parent().expect("report parent")).expect("report directory");
    let captures = [
        ("opened", opened),
        ("typed", typed),
        ("resized", resized),
        ("interrupted", interrupted),
        ("exited", exited),
    ];
    let evidence = captures
        .into_iter()
        .map(|(stage, frame)| {
            json!({"stage":stage, "rows":rows(&frame), "frame":frame}).to_string() + "\n"
        })
        .collect::<String>();
    std::fs::write(report, evidence).expect("actual surface captures");
}
