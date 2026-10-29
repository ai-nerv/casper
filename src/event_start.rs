use casper::events::forwarding::{Forwarder, receive_stdin};

pub const CAPABILITIES: &[u8] = b"{\"ok\":true,\"family\":1,\"n\":1,\"result\":[{\"schema\":1,\"kind\":\"native_event_capabilities\",\"program\":\"casper\",\"bootstrap\":2}]}\n";

pub fn start(args: &mut Vec<String>) -> Result<Option<Forwarder>, &'static str> {
    if !requested(args)? {
        return Ok(None);
    }
    let sender = receive_stdin().map_err(|_| "invalid native event bootstrap")?;
    Ok(Forwarder::activate(sender).ok())
}

fn requested(args: &mut Vec<String>) -> Result<bool, &'static str> {
    let count = args
        .iter()
        .filter(|arg| *arg == "--event-bootstrap")
        .count();
    if count == 0 {
        return Ok(false);
    }
    if count != 1 || !matches!(args.first().map(String::as_str), Some("run" | "serve")) {
        return Err("native event bootstrap requires one flag on run or serve");
    }
    args.retain(|arg| arg != "--event-bootstrap");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_explicit_native_commands_activate() {
        for command in ["run", "serve"] {
            let mut args = vec![command.into(), "--event-bootstrap".into(), "--cbor".into()];
            assert_eq!(requested(&mut args), Ok(true));
            assert_eq!(args, [command, "--cbor"]);
        }
        for args in [
            vec!["tools", "--event-bootstrap"],
            vec!["--event-bootstrap", "run"],
            vec!["run", "--event-bootstrap", "--event-bootstrap"],
        ] {
            let mut args = args.into_iter().map(String::from).collect();
            assert!(requested(&mut args).is_err());
        }
        assert_eq!(requested(&mut vec!["run".into()]), Ok(false));
    }
}
