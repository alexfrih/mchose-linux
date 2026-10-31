//! Feature-report access to a hidraw node, straight on the ioctls.
//! No hidapi, so the binary has no C dependency beyond libc.

use std::ffi::c_int;
use std::fs::{self, File};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
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
        // Nonblocking so input reports can be polled with a timeout (the G3
        // family talks over interrupt OUT/IN, not feature reports).
        let file = File::options()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)?;
        Ok(Self { file, path: path.to_path_buf() })
    }

    /// Interrupt OUT: one frame, `payload[0]` must already hold the report ID
    /// (0 when the interface declares no report IDs).
    pub fn write_report(&self, payload: &[u8]) -> io::Result<()> {
        use std::io::Write;
        let n = (&self.file).write(payload)?;
        if n != payload.len() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                format!("short write: {n} of {} bytes", payload.len()),
            ));
        }
        Ok(())
    }

    /// Interrupt IN: wait up to `timeout` for one input report.
    pub fn read_report(&self, timeout: std::time::Duration) -> io::Result<Vec<u8>> {
        let mut pfd = libc::pollfd {
            fd: self.file.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        if rc == 0 {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "no input report"));
        }
        let mut buf = vec![0u8; 256];
        let n = unsafe {
            libc::read(
                self.file.as_raw_fd(),
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        buf.truncate(n as usize);
        Ok(buf)
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

/// True when the descriptor declares any Feature report. The two protocol
/// families split on this: L7-family mice configure through feature reports
/// 0x11/0x12, while the G3 family declares only 64-byte interrupt IN/OUT
/// reports and answers frames on those.
pub fn has_feature_report(descriptor: &[u8]) -> bool {
    let mut i = 0;
    while i < descriptor.len() {
        let b = descriptor[i];
        if b == 0xFE {
            // Long item: length byte, tag byte, then the data.
            let len = descriptor.get(i + 1).copied().unwrap_or(0) as usize;
            i += 3 + len;
            continue;
        }
        let size = match b & 0x03 {
            3 => 4,
            s => s as usize,
        };
        if b & 0xFC == 0xB0 {
            // Main item, tag 1011: Feature.
            return true;
        }
        i += 1 + size;
    }
    false
}
