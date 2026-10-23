// Ninimma modification: independent project branding and integration.
//! The per-user index folder, and keeping other users out of it.
//!
//! Unix: the folders are mode 0700 and the files 0600, owned by this user,
//! and never symlinks. Windows: the folder gets an explicit, non-inherited
//! access list naming only this user and SYSTEM, which its files inherit.
//!
//! This keeps other normal users out. It can't stop an administrator/root,
//! or malware running as this user while the index is unlocked.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermError {
    /// The location exists but is a symlink (or junction), which could
    /// point the index somewhere other users can read.
    Symlink,
    /// Owned by another user.
    #[cfg_attr(not(unix), allow(dead_code))]
    WrongOwner,
    /// Exists but is the wrong kind of thing (a file where a folder goes).
    WrongType,
    /// The access rules couldn't be set or checked.
    #[cfg_attr(not(windows), allow(dead_code))]
    Insecure,
    Io(io::ErrorKind),
}

impl fmt::Display for PermError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PermError::Symlink => f.write_str("the index location is a link, which isn't allowed"),
            PermError::WrongOwner => f.write_str("the index location belongs to another user"),
            PermError::WrongType => {
                f.write_str("the index location isn't the expected kind of item")
            }
            PermError::Insecure => {
                f.write_str("the index location's permissions couldn't be made private")
            }
            PermError::Io(kind) => write!(f, "the index location couldn't be accessed ({kind})"),
        }
    }
}

impl std::error::Error for PermError {}

impl From<io::Error> for PermError {
    fn from(e: io::Error) -> Self {
        PermError::Io(e.kind())
    }
}

/// Where this user's index lives: local and non-roaming, never shared,
/// synced or inside the installation.
pub fn index_dir() -> Option<PathBuf> {
    index_dir_from(|name| std::env::var_os(name))
}

fn index_dir_from(env: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    let absolute = |v: std::ffi::OsString| Some(PathBuf::from(v)).filter(|p| p.is_absolute());
    if cfg!(windows) {
        let local = env("LOCALAPPDATA").and_then(absolute)?;
        Some(local.join("Ninimma").join("Index"))
    } else if cfg!(target_os = "macos") {
        let home = env("HOME").and_then(absolute)?;
        Some(home.join("Library/Application Support/Ninimma/Index"))
    } else {
        // XDG: a relative XDG_STATE_HOME is invalid and must be ignored.
        let state = env("XDG_STATE_HOME").and_then(absolute).or_else(|| {
            env("HOME")
                .and_then(absolute)
                .map(|h| h.join(".local/state"))
        })?;
        Some(state.join("ninimma").join("index"))
    }
}

/// Creates the index folder and its app folder privately, or checks the
/// existing ones. Folders above those are ordinary (e.g. ~/.local/state).
pub fn prepare_index_dir(dir: &Path) -> Result<(), PermError> {
    let app_dir = dir.parent().ok_or(PermError::WrongType)?;
    if let Some(base) = app_dir.parent() {
        fs::create_dir_all(base)?;
    }
    for d in [app_dir, dir] {
        make_private_dir(d)?;
    }
    #[cfg(windows)]
    windows_acl::protect(dir)?;
    Ok(())
}

/// Creates `dir` private to this user, or checks (and if it's ours,
/// tightens) the existing one.
fn make_private_dir(dir: &Path) -> Result<(), PermError> {
    match fs::symlink_metadata(dir) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            #[cfg(unix)]
            let builder = {
                let mut b = fs::DirBuilder::new();
                std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
                b
            };
            #[cfg(not(unix))]
            let builder = fs::DirBuilder::new();
            match builder.create(dir) {
                Ok(()) => {}
                // Created meanwhile; checked below like any existing folder.
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.into()),
            }
        }
        Err(e) => return Err(e.into()),
    }
    check_private(dir, true)
}

/// Checks an existing index file, tightening its mode if it's ours.
pub fn check_private_file(path: &Path) -> Result<(), PermError> {
    check_private(path, false)
}

/// Creates a new file that only this user can read (it must not exist).
pub fn create_private_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

/// The safe-repair rule: only something we own and that isn't a link has
/// its mode tightened. Anything else is rejected, never changed.
fn check_private(path: &Path, want_dir: bool) -> Result<(), PermError> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() || is_reparse_point(&meta) {
        return Err(PermError::Symlink);
    }
    if meta.is_dir() != want_dir || (!want_dir && !meta.is_file()) {
        return Err(PermError::WrongType);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        // SAFETY: geteuid has no preconditions.
        if meta.uid() != unsafe { libc::geteuid() } {
            return Err(PermError::WrongOwner);
        }
        let wanted = if want_dir { 0o700 } else { 0o600 };
        if meta.mode() & 0o777 != wanted {
            fs::set_permissions(path, fs::Permissions::from_mode(wanted))?;
        }
    }
    Ok(())
}

#[cfg(windows)]
fn is_reparse_point(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_meta: &fs::Metadata) -> bool {
    false
}

#[cfg(windows)]
mod windows_acl {
    //! An explicit access list on the index folder: full control for this
    //! user and SYSTEM only, inherited by everything inside, and protected
    //! from inheriting broader rules from the parent folders.

    use super::PermError;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::{
        CloseHandle, LocalFree, ERROR_SUCCESS, GENERIC_ALL, HANDLE,
    };
    use windows_sys::Win32::Security::Authorization::{
        SetEntriesInAclW, SetNamedSecurityInfoW, EXPLICIT_ACCESS_W, NO_MULTIPLE_TRUSTEE,
        SET_ACCESS, SE_FILE_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_IS_WELL_KNOWN_GROUP,
        TRUSTEE_W,
    };
    use windows_sys::Win32::Security::{
        CreateWellKnownSid, GetTokenInformation, TokenUser, WinLocalSystemSid, ACL,
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, SECURITY_MAX_SID_SIZE,
        SUB_CONTAINERS_AND_OBJECTS_INHERIT, TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    pub fn protect(dir: &Path) -> Result<(), PermError> {
        // SAFETY: plain Win32 calls; every buffer outlives the calls that
        // use it, and the ACL from SetEntriesInAclW is freed with LocalFree.
        unsafe {
            let mut token: HANDLE = null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(PermError::Insecure);
            }
            let mut len = 0u32;
            GetTokenInformation(token, TokenUser, null_mut(), 0, &mut len);
            // u64 elements keep the TOKEN_USER suitably aligned.
            let mut user = vec![0u64; (len as usize).div_ceil(8)];
            let ok = GetTokenInformation(token, TokenUser, user.as_mut_ptr().cast(), len, &mut len);
            CloseHandle(token);
            if ok == 0 {
                return Err(PermError::Insecure);
            }
            let user_sid = (*(user.as_ptr() as *const TOKEN_USER)).User.Sid;

            let mut system = vec![0u64; (SECURITY_MAX_SID_SIZE as usize).div_ceil(8)];
            let mut system_len = SECURITY_MAX_SID_SIZE;
            if CreateWellKnownSid(
                WinLocalSystemSid,
                null_mut(),
                system.as_mut_ptr().cast(),
                &mut system_len,
            ) == 0
            {
                return Err(PermError::Insecure);
            }

            let access = |sid: *mut core::ffi::c_void, kind| EXPLICIT_ACCESS_W {
                grfAccessPermissions: GENERIC_ALL,
                grfAccessMode: SET_ACCESS,
                grfInheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT,
                Trustee: TRUSTEE_W {
                    pMultipleTrustee: null_mut(),
                    MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                    TrusteeForm: TRUSTEE_IS_SID,
                    TrusteeType: kind,
                    ptstrName: sid.cast(),
                },
            };
            let entries = [
                access(user_sid, TRUSTEE_IS_USER),
                access(system.as_mut_ptr().cast(), TRUSTEE_IS_WELL_KNOWN_GROUP),
            ];
            let mut acl: *mut ACL = null_mut();
            if SetEntriesInAclW(entries.len() as u32, entries.as_ptr(), null(), &mut acl)
                != ERROR_SUCCESS
            {
                return Err(PermError::Insecure);
            }
            let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
            let result = SetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                acl,
                null(),
            );
            LocalFree(acl.cast());
            if result != ERROR_SUCCESS {
                return Err(PermError::Insecure);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn index_dir_follows_platform_rules() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| OsString::from(v))
            }
        };
        if cfg!(windows) {
            let dir =
                index_dir_from(env(&[("LOCALAPPDATA", r"C:\Users\a\AppData\Local")])).unwrap();
            assert_eq!(dir, Path::new(r"C:\Users\a\AppData\Local\Ninimma\Index"));
        } else if cfg!(target_os = "macos") {
            let dir = index_dir_from(env(&[("HOME", "/Users/a")])).unwrap();
            assert_eq!(
                dir,
                Path::new("/Users/a/Library/Application Support/Ninimma/Index")
            );
        } else {
            let dir = index_dir_from(env(&[("HOME", "/home/a")])).unwrap();
            assert_eq!(dir, Path::new("/home/a/.local/state/ninimma/index"));
            let dir =
                index_dir_from(env(&[("HOME", "/home/a"), ("XDG_STATE_HOME", "/st")])).unwrap();
            assert_eq!(dir, Path::new("/st/ninimma/index"));
            // A relative XDG_STATE_HOME is ignored, as the spec requires.
            let dir =
                index_dir_from(env(&[("HOME", "/home/a"), ("XDG_STATE_HOME", "rel")])).unwrap();
            assert_eq!(dir, Path::new("/home/a/.local/state/ninimma/index"));
            assert_eq!(index_dir_from(env(&[])), None);
        }
    }

    #[cfg(unix)]
    #[test]
    fn folders_are_0700_and_files_0600() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state/ninimma/index");
        prepare_index_dir(&dir).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(dir.parent().unwrap()), 0o700);
        let file = dir.join("metadata.db");
        create_private_file(&file).unwrap();
        assert_eq!(mode(&file), 0o600);
        // A loosened folder or file of ours is tightened again.
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        prepare_index_dir(&dir).unwrap();
        check_private_file(&file).unwrap();
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&file), 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_folders_and_files_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        let app = tmp.path().join("ninimma");
        fs::create_dir(&app).unwrap();
        std::os::unix::fs::symlink(&elsewhere, app.join("index")).unwrap();
        assert_eq!(
            prepare_index_dir(&app.join("index")),
            Err(PermError::Symlink)
        );

        let real = tmp.path().join("real.db");
        fs::write(&real, b"").unwrap();
        let link = tmp.path().join("link.db");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(check_private_file(&link), Err(PermError::Symlink));
    }

    #[test]
    fn a_file_where_the_folder_goes_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("ninimma");
        fs::create_dir(&app).unwrap();
        fs::write(app.join("index"), b"").unwrap();
        assert_eq!(
            prepare_index_dir(&app.join("index")),
            Err(PermError::WrongType)
        );
    }

    #[test]
    fn private_files_are_never_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("vault");
        create_private_file(&file).unwrap();
        assert!(create_private_file(&file).is_err());
    }
}
