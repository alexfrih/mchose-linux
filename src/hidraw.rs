//! Feature-report access to a hidraw node, straight on the ioctls.
//! No hidapi, so the binary has no C dependency beyond libc.

use std::ffi::c_int;
use std::fs::{self, File};
use std::io;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;

const fn ioc(dir: u32, ty: u32, nr: u32, size: u32) -> u64 {
    ((dir << 30) | (size << 16) | (ty << 8) | nr) as u64
}
fn hidioc_sfeature(len: u32) -> u64 {
    ioc(IOC_WRITE | IOC_READ, b'H' as u32, 0x06, len)
}
fn hidioc_gfeature(len: u32) -> u64 {
    ioc(IOC_WRITE | IOC_READ, b'H' as u32, 0x07, len)
}

pub struct HidRaw {
    file: File,
    #[allow(dead_code)]
    pub path: PathBuf,
}

impl HidRaw {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::options().read(true).write(true).open(path)?;
        Ok(Self { file, path: path.to_path_buf() })
    }

    /// SET_REPORT(Feature). `buf[0]` must already hold the report ID.
    pub fn set_feature(&self, buf: &[u8]) -> io::Result<()> {
        let rc = unsafe {
            libc::ioctl(
                self.file.as_raw_fd(),
                hidioc_sfeature(buf.len() as u32),
                buf.as_ptr(),
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// GET_REPORT(Feature). `buf[0]` carries the requested report ID in, the
    /// whole report out.
    pub fn get_feature(&self, buf: &mut [u8]) -> io::Result<usize> {
        let rc: c_int = unsafe {
            libc::ioctl(
                self.file.as_raw_fd(),
                hidioc_gfeature(buf.len() as u32),
                buf.as_mut_ptr(),
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(rc as usize)
    }
}

/// One candidate hidraw node, with what sysfs says about it.
pub struct Node {
    pub dev: PathBuf,
    pub vid: u16,
    pub pid: u16,
    pub name: String,
    pub descriptor: Vec<u8>,
}

/// Every hidraw node on the machine, cheapest fields first.
pub fn nodes() -> io::Result<Vec<Node>> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/class/hidraw") else {
        return Ok(out);
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let sys = entry.path();
        let uevent = fs::read_to_string(sys.join("device/uevent")).unwrap_or_default();
        let mut vid = 0u16;
        let mut pid = 0u16;
        let mut name = String::new();
        for line in uevent.lines() {
            if let Some(id) = line.strip_prefix("HID_ID=") {
                // bus:vendor:product, all hex, vendor and product zero-padded to 8
                let mut parts = id.split(':');
                parts.next();
                vid = parts.next().and_then(|v| u32::from_str_radix(v, 16).ok()).unwrap_or(0) as u16;
                pid = parts.next().and_then(|v| u32::from_str_radix(v, 16).ok()).unwrap_or(0) as u16;
            } else if let Some(n) = line.strip_prefix("HID_NAME=") {
                name = n.trim().to_string();
            }
        }
        let descriptor = fs::read(sys.join("device/report_descriptor")).unwrap_or_default();
        out.push(Node {
            dev: PathBuf::from("/dev").join(entry.file_name()),
            vid,
            pid,
            name,
            descriptor,
        });
    }
    Ok(out)
}

/// True when the descriptor declares the vendor usage page the config
/// protocol lives on: `06 01 FF` = Usage Page (Vendor 0xFF01).
pub fn has_config_collection(descriptor: &[u8]) -> bool {
    descriptor.windows(3).any(|w| w == [0x06, 0x01, 0xFF])
}
