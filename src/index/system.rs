// Ninimma modification: independent project branding and integration.
//! What the indexer asks the system before doing work: battery, load,
//! free disk space, network locations; and lowering its own priority.
//!
//! Where a platform has no cheap answer the check returns "fine" rather
//! than guessing, and the docs say which checks each platform supports.

use std::path::Path;

/// Whether the computer is running on battery. `false` when unknown or on
/// a desktop.
pub fn on_battery() -> bool {
    platform::on_battery()
}

/// Whether the system is busy enough that indexing should wait: load
/// above the number of cores, or under 10% of memory available. Linux only.
pub fn under_pressure() -> bool {
    // Unrelated CI builds must not pause index workers in deterministic unit tests.
    !cfg!(test) && platform::under_pressure()
}

/// Free bytes on the disk holding `path`.
pub fn free_space(path: &Path) -> Option<u64> {
    platform::free_space(path)
}

/// Whether `path` is on a network share or remote mount.
pub fn is_network_path(path: &Path) -> bool {
    platform::is_network_path(path)
}

/// Runs the calling thread at background priority where the platform
/// allows it per thread (Linux, macOS, Windows).
pub fn lower_thread_priority() {
    platform::lower_thread_priority();
}

/// Network filesystems as named in the mount table (FreeBSD's names are
/// translated to these by `file_system::mount_table`).
#[cfg(any(all(unix, not(target_os = "macos")), test))]
fn is_network_fs(fs_type: &str) -> bool {
    matches!(
        fs_type,
        "nfs"
            | "nfs4"
            | "cifs"
            | "smb3"
            | "smbfs"
            | "9p"
            | "davfs"
            | "fuse.sshfs"
            | "fuse.rclone"
            | "fuse.gvfsd-fuse"
            | "fuse.kio-fuse"
            | "fuse.s3fs"
            | "afpfs"
    )
}

/// The filesystem type of the mount holding `path`, from a mount table in
/// /proc/self/mounts format: the longest mount point that contains it.
#[cfg(any(all(unix, not(target_os = "macos")), test))]
fn mount_type_of<'a>(mounts: &'a str, path: &Path) -> Option<&'a str> {
    mounts
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let (_, point, fs_type) = (parts.next()?, parts.next()?, parts.next()?);
            let point = point.replace("\\040", " ");
            path.starts_with(&point).then_some((point.len(), fs_type))
        })
        .max_by_key(|(len, _)| *len)
        .map(|(_, fs_type)| fs_type)
}

#[cfg(target_os = "linux")]
mod platform {
    use std::fs;
    use std::path::Path;

    pub fn on_battery() -> bool {
        let Ok(entries) = fs::read_dir("/sys/class/power_supply") else {
            return false;
        };
        let (mut has_battery, mut mains_online) = (false, false);
        for entry in entries.flatten() {
            let read = |name: &str| fs::read_to_string(entry.path().join(name)).unwrap_or_default();
            match read("type").trim() {
                "Battery" => has_battery |= read("scope").trim() != "Device",
                "Mains" | "USB" => mains_online |= read("online").trim() == "1",
                _ => {}
            }
        }
        has_battery && !mains_online
    }

    pub fn under_pressure() -> bool {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as f64;
        let load = fs::read_to_string("/proc/loadavg")
            .ok()
            .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
            .unwrap_or(0.0);
        let meminfo = fs::read_to_string("/proc/meminfo").unwrap_or_default();
        let field = |name: &str| -> Option<f64> {
            meminfo
                .lines()
                .find(|l| l.starts_with(name))?
                .split_whitespace()
                .nth(1)?
                .parse()
                .ok()
        };
        let low_memory = match (field("MemAvailable:"), field("MemTotal:")) {
            (Some(avail), Some(total)) if total > 0.0 => avail / total < 0.10,
            _ => false,
        };
        load > cores * 1.5 || low_memory
    }

    pub fn free_space(path: &Path) -> Option<u64> {
        super::statvfs_free(path)
    }

    pub fn is_network_path(path: &Path) -> bool {
        let mounts = crate::file_system::mount_table();
        super::mount_type_of(&mounts, path).is_some_and(super::is_network_fs)
    }

    pub fn lower_thread_priority() {
        // On Linux, nice applies to the calling thread.
        // SAFETY: plain syscalls on our own thread.
        unsafe {
            let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
            libc::setpriority(libc::PRIO_PROCESS, tid, 19);
        }
    }
}

#[cfg(target_os = "freebsd")]
mod platform {
    use std::path::Path;

    pub fn on_battery() -> bool {
        // hw.acpi.acline: 1 on AC power, 0 on battery; missing on desktops.
        let mut value: libc::c_int = 1;
        let mut len = std::mem::size_of::<libc::c_int>();
        // SAFETY: the name is NUL-terminated and value/len describe an int.
        let ok = unsafe {
            libc::sysctlbyname(
                c"hw.acpi.acline".as_ptr(),
                (&mut value as *mut libc::c_int).cast(),
                &mut len,
                std::ptr::null(),
                0,
            )
        } == 0;
        ok && value == 0
    }

    pub fn under_pressure() -> bool {
        false
    }

    pub fn free_space(path: &Path) -> Option<u64> {
        super::statvfs_free(path)
    }

    pub fn is_network_path(path: &Path) -> bool {
        let mounts = crate::file_system::mount_table();
        super::mount_type_of(&mounts, path).is_some_and(super::is_network_fs)
    }

    /// FreeBSD sets priority per process, which would slow the whole app,
    /// so the indexer relies on its own pacing instead.
    pub fn lower_thread_priority() {}
}

#[cfg(target_os = "macos")]
mod platform {
    use std::path::Path;
    use std::process::Command;

    pub fn on_battery() -> bool {
        // `pmset` with fixed arguments; no shell, no user data.
        Command::new("/usr/bin/pmset")
            .args(["-g", "batt"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("'Battery Power'"))
            .unwrap_or(false)
    }

    pub fn under_pressure() -> bool {
        false
    }

    pub fn free_space(path: &Path) -> Option<u64> {
        super::statvfs_free(path)
    }

    pub fn is_network_path(path: &Path) -> bool {
        use std::os::unix::ffi::OsStrExt;
        let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: c is NUL-terminated; st is written by statfs.
        unsafe {
            let mut st: libc::statfs = std::mem::zeroed();
            libc::statfs(c.as_ptr(), &mut st) == 0 && st.f_flags & libc::MNT_LOCAL as u32 == 0
        }
    }

    pub fn lower_thread_priority() {
        // SAFETY: sets the QoS class of the calling thread only.
        unsafe {
            libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_BACKGROUND, 0);
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Component, Path, Prefix};
    use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW};
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    use windows_sys::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
    };

    const DRIVE_REMOTE: u32 = 4;

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    pub fn on_battery() -> bool {
        // SAFETY: status is written by the call.
        unsafe {
            let mut status: SYSTEM_POWER_STATUS = std::mem::zeroed();
            GetSystemPowerStatus(&mut status) != 0 && status.ACLineStatus == 0
        }
    }

    pub fn under_pressure() -> bool {
        false
    }

    pub fn free_space(path: &Path) -> Option<u64> {
        let mut free = 0u64;
        // SAFETY: the path is NUL-terminated; free is written by the call.
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide(path).as_ptr(),
                &mut free,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        (ok != 0).then_some(free)
    }

    pub fn is_network_path(path: &Path) -> bool {
        match path.components().next() {
            Some(Component::Prefix(p)) => match p.kind() {
                Prefix::UNC(..) | Prefix::VerbatimUNC(..) => true,
                Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                    let root: Vec<u16> = format!("{}:\\", letter as char)
                        .encode_utf16()
                        .chain(Some(0))
                        .collect();
                    // SAFETY: root is NUL-terminated.
                    unsafe { GetDriveTypeW(root.as_ptr()) == DRIVE_REMOTE }
                }
                _ => false,
            },
            _ => false,
        }
    }

    pub fn lower_thread_priority() {
        // Background mode lowers CPU, disk and memory priority together.
        // SAFETY: applies to the calling thread only.
        unsafe {
            SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN);
        }
    }
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "freebsd",
    target_os = "macos",
    windows
)))]
mod platform {
    use std::path::Path;
    pub fn on_battery() -> bool {
        false
    }
    pub fn under_pressure() -> bool {
        false
    }
    pub fn free_space(_path: &Path) -> Option<u64> {
        None
    }
    pub fn is_network_path(_path: &Path) -> bool {
        false
    }
    pub fn lower_thread_priority() {}
}

#[cfg(unix)]
fn statvfs_free(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: c is NUL-terminated; st is written by statvfs.
    unsafe {
        let mut st: libc::statvfs = std::mem::zeroed();
        (libc::statvfs(c.as_ptr(), &mut st) == 0).then(|| st.f_bavail as u64 * st.f_frsize as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_mounts_are_recognized_by_the_longest_mount_point() {
        let mounts = "/dev/sda1 / ext4 rw 0 0\n\
                      //nas/photos /mnt/photos cifs rw 0 0\n\
                      /dev/sdb1 /mnt/photos/local\\040disk ext4 rw 0 0\n\
                      gvfsd-fuse /run/user/1000/gvfs fuse.gvfsd-fuse rw 0 0\n";
        let kind = |p: &str| mount_type_of(mounts, Path::new(p));
        assert_eq!(kind("/home/a"), Some("ext4"));
        assert_eq!(kind("/mnt/photos/2025"), Some("cifs"));
        assert_eq!(kind("/mnt/photos/local disk/x"), Some("ext4"));
        assert!(is_network_fs(kind("/mnt/photos/2025").unwrap()));
        assert!(is_network_fs(
            kind("/run/user/1000/gvfs/smb-share:server=nas").unwrap()
        ));
        assert!(!is_network_fs(kind("/home/a").unwrap()));
    }

    #[test]
    fn system_checks_answer_without_panicking() {
        let _ = on_battery();
        assert!(
            !under_pressure(),
            "unit tests must ignore unrelated host load"
        );
        assert!(!is_network_path(&std::env::temp_dir()));
        #[cfg(any(unix, windows))]
        assert!(free_space(&std::env::temp_dir()).is_some());
    }
}
