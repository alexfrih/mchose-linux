//! Node discovery and probing.
//!
//! Two protocol families show up under the MCHOSE vendor IDs, split by the
//! report descriptor: L7-family mice declare feature reports on the vendor
//! collection, the G3 family declares only 64-byte interrupt IN/OUT reports.

use crate::g3;
use crate::hidraw::{self, HidRaw, Node};
use crate::logln;
use crate::proto;
use std::io;
use std::path::Path;

/// Vendor IDs we have seen this protocol family on: the L7 line and the
/// newer ODM one (G3 V2 and friends, per the vendor's web driver).
pub const VENDORS: [u16; 2] = [0x5253, 0x3837];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Family {
    L7,
    G3,
}

/// An opened, probed mouse.
pub struct Mouse {
    pub dev: HidRaw,
    pub family: Family,
    pub vid: u16,
    pub pid: u16,
    pub name: String,
}

impl Mouse {
    fn new(dev: HidRaw, family: Family, node: &Node) -> Self {
        Self {
            dev,
            family,
            vid: node.vid,
            pid: node.pid,
            name: node.name.clone(),
        }
    }
}

/// Nodes whose report descriptor has the vendor collection (usage page
/// 0xFF01) this protocol speaks.
pub fn candidates() -> io::Result<Vec<Node>> {
    let nodes = hidraw::nodes()?;
    Ok(nodes
        .into_iter()
        .filter(|n| VENDORS.contains(&n.vid) && hidraw::has_config_collection(&n.descriptor))
        .collect())
}

fn family_of(node: &Node) -> Family {
    if hidraw::has_feature_report(&node.descriptor) {
        Family::L7
    } else {
        Family::G3
    }
}

/// Protocol split as above. `Err` carries every per-node failure so a
/// half-broken udev setup does not hide behind a bare "not found".
pub fn open() -> io::Result<Mouse> {
    let mut found = candidates()?;
    if found.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no MCHOSE configuration interface found (vid 5253 or 3837, vendor collection 0xff01)",
        ));
    }
    // Prefer the L7 2.4 GHz dongle and the wired G3 V2: the L7 mouse itself
    // answers some reads with all 0xff through its own interface, and the G3
    // dongle serves a stale config while the mouse is also on the cable.
    found.sort_by_key(|n| n.pid != 0x00b0 && n.pid != 0x418c);
    let mut failures = Vec::new();
    for n in &found {
        let dev = match HidRaw::open(&n.dev) {
            Ok(d) => d,
            Err(e) => {
                failures.push(format!("{}: {e}", n.dev.display()));
                continue;
            }
        };
        match probe(&dev, family_of(n)) {
            Ok(family) => return Ok(Mouse::new(dev, family, n)),
            Err(e) => failures.push(format!("{}: {}", n.dev.display(), refused(&e))),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotConnected,
        format!("found {} candidate(s), none answered: {}", found.len(), failures.join("; ")),
    ))
}

/// An explicitly named node (`--device /dev/hidrawN`), probed the same way.
/// When sysfs knows the node the descriptor picks the family; otherwise both
/// probes run, L7 first since a G3 device refuses feature reports fast.
pub fn open_path(path: &Path) -> io::Result<Mouse> {
    let dev = HidRaw::open(path)?;
    let node = hidraw::nodes()?
        .into_iter()
        .find(|n| n.dev == path)
        .unwrap_or_else(|| Node {
            dev: path.to_path_buf(),
            vid: 0,
            pid: 0,
            name: String::new(),
            descriptor: Vec::new(),
        });
    let first = if node.descriptor.is_empty() {
        Family::L7
    } else {
        family_of(&node)
    };
    let second = match first {
        Family::L7 => Family::G3,
        Family::G3 => Family::L7,
    };
    if probe(&dev, first).is_ok() {
        return Ok(Mouse::new(dev, first, &node));
    }
    probe(&dev, second)?;
    Ok(Mouse::new(dev, second, &node))
}

/// The actual wire check for each family.
fn probe(dev: &HidRaw, family: Family) -> io::Result<Family> {
    match family {
        Family::L7 => {
            let id = proto::identity(dev)?;
            logln!("device {:04x}:{:04x} answered (L7 family)", id.vid, id.pid);
        }
        Family::G3 => {
            g3::version(dev)?;
            logln!("device answered (G3 family)");
        }
    }
    Ok(family)
}

/// Translate the raw hidraw error into why that node cannot be the one.
/// The G3 family's vendor interface declares no feature reports, so every
/// GET_REPORT stalls with EPIPE(32).
fn refused(e: &io::Error) -> String {
    match e.raw_os_error() {
        Some(32) => "device refuses feature reports (unsupported model)".into(),
        Some(13) => "permission denied (install 70-mchose.rules)".into(),
        _ => e.to_string(),
    }
}
