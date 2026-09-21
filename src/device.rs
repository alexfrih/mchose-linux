//! Shared discovery for the CLI and GUI. A USB receiver is not a live mouse.
use crate::{hidraw::{self, HidRaw, Node}, proto};
use std::io;

pub fn candidates() -> io::Result<Vec<Node>> {
    Ok(hidraw::nodes()?.into_iter().filter(|n|
        matches!(n.vid, 0x5253 | 0x3837) && hidraw::has_config_collection(&n.descriptor)
    ).collect())
}

fn first_responsive<T>(nodes: Vec<Node>, mut probe: impl FnMut(&Node) -> io::Result<T>) -> io::Result<T> {
    let mut failures = Vec::new();
    for node in nodes {
        match probe(&node) {
            Ok(device) => return Ok(device),
            Err(error) => failures.push(format!("{}: {error}", node.dev.display())),
        }
    }
    let detail = if failures.is_empty() {
        "Connect the mouse by USB cable or its receiver.".to_string()
    } else { failures.join("; ") };
    Err(io::Error::new(io::ErrorKind::NotConnected, format!("No responding MCHOSE mouse. {detail}")))
}

pub fn open() -> io::Result<HidRaw> {
    let mut found = candidates()?;
    // This known L7 wired interface answers even while its separate receiver
    // is idle. Other models still get probed; no settings are written here.
    found.sort_by_key(|n| n.pid != 0x00b0);
    first_responsive(found, |node| {
        let dev = HidRaw::open(&node.dev)?;
        let identity = proto::identity(&dev)?;
        if identity.connect_mode == 1 && !identity.connected {
            return Err(io::Error::new(io::ErrorKind::NotConnected, "receiver has no active mouse"));
        }
        Ok(dev)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn node(path: &str) -> Node {
        Node { dev: path.into(), vid: 0x5253, pid: 0, name: String::new(), descriptor: vec![] }
    }
    #[test]
    fn idle_receiver_does_not_hide_wired_mouse() {
        let mut attempts = Vec::new();
        let selected = first_responsive(vec![node("receiver"), node("mouse")], |n| {
            attempts.push(n.dev.clone());
            if n.dev.to_str() == Some("receiver") { Err(io::ErrorKind::NotConnected.into()) }
            else { Ok(n.dev.clone()) }
        }).unwrap();
        assert_eq!(selected.to_str(), Some("mouse"));
        assert_eq!(attempts.len(), 2);
    }
    #[test]
    fn no_device_is_an_error_not_empty_success() {
        assert!(first_responsive::<()>(vec![], |_| Ok(())).is_err());
    }
    #[test]
    fn inaccessible_device_does_not_hide_next_device() {
        let selected = first_responsive(vec![node("denied"), node("ok")], |n| {
            if n.dev.to_str() == Some("denied") { Err(io::ErrorKind::PermissionDenied.into()) }
            else { Ok(n.dev.clone()) }
        }).unwrap();
        assert_eq!(selected.to_str(), Some("ok"));
    }
}
