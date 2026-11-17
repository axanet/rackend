//! The data directory (SPEC §5.3): creation, write check, exclusive lock,
//! `tmp/` wipe and the network-filesystem warning.

use std::fs::{self, File, TryLockError};
use std::path::{Path, PathBuf};

use crate::exit::Fatal;

/// The prepared data directory. Holds the exclusive lock until dropped.
#[derive(Debug)]
pub struct DataDir {
    root: PathBuf,
    lock: File,
}

impl DataDir {
    /// Creates the directory if missing, checks it is writable, acquires the
    /// exclusive lock and wipes `tmp/`.
    ///
    /// The lock is taken **before** wiping `tmp/` so a second instance can
    /// never delete the first one's upload staging files (ADR 0004).
    pub fn prepare(root: &Path) -> Result<Self, Fatal> {
        fs::create_dir_all(root).map_err(|e| {
            Fatal::startup(format!(
                "cannot create data directory {}: {e}",
                root.display()
            ))
        })?;
        let root = root.canonicalize().map_err(|e| {
            Fatal::startup(format!(
                "cannot resolve data directory {}: {e}",
                root.display()
            ))
        })?;
        check_writable(&root)?;

        let lock = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(root.join("rackend.lock"))
            .map_err(|e| {
                Fatal::startup(format!(
                    "cannot open {}: {e}",
                    root.join("rackend.lock").display()
                ))
            })?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(Fatal::locked(format!(
                    "another rackend instance is using {}",
                    root.display()
                )));
            }
            Err(TryLockError::Error(e)) => {
                return Err(Fatal::startup(format!(
                    "cannot lock {}: {e}",
                    root.display()
                )));
            }
        }

        let tmp = root.join("tmp");
        if tmp.exists() {
            fs::remove_dir_all(&tmp)
                .map_err(|e| Fatal::startup(format!("cannot wipe {}: {e}", tmp.display())))?;
        }
        for dir in [tmp, root.join("backups"), root.join("storage")] {
            fs::create_dir_all(&dir)
                .map_err(|e| Fatal::startup(format!("cannot create {}: {e}", dir.display())))?;
        }
        Ok(Self { root, lock })
    }

    /// The (canonical) root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `data.db`.
    pub fn data_db(&self) -> PathBuf {
        self.root.join("data.db")
    }

    /// `logs.db`.
    pub fn logs_db(&self) -> PathBuf {
        self.root.join("logs.db")
    }

    /// `storage/`: the filesystem blob backend.
    pub fn storage(&self) -> PathBuf {
        self.root.join("storage")
    }

    /// `tmp/`: uploads in flight; wiped on start.
    pub fn tmp(&self) -> PathBuf {
        self.root.join("tmp")
    }

    /// `backups/`.
    pub fn backups(&self) -> PathBuf {
        self.root.join("backups")
    }

    /// Releases the lock (also happens on drop).
    pub fn release(self) {
        let _ = self.lock.unlock();
    }
}

fn check_writable(root: &Path) -> Result<(), Fatal> {
    let probe = root.join(format!(".write-test-{}", std::process::id()));
    fs::write(&probe, b"ok")
        .and_then(|()| fs::remove_file(&probe))
        .map_err(|e| {
            Fatal::startup(format!(
                "data directory {} is not writable: {e}",
                root.display()
            ))
        })
}

/// The kind of network filesystem the directory is on, if any (Linux only).
pub fn network_filesystem(path: &Path) -> Option<&'static str> {
    imp::network_filesystem(path)
}

#[cfg(target_os = "linux")]
mod imp {
    use std::path::Path;

    // statfs(2) f_type magic numbers.
    const NFS: u64 = 0x6969;
    const SMB: u64 = 0x517B;
    const CIFS: u64 = 0xFF53_4D42;
    const SMB2: u64 = 0xFE53_4D42;
    const FUSE: u64 = 0x6573_5546;

    pub fn network_filesystem(path: &Path) -> Option<&'static str> {
        let st = rustix::fs::statfs(path).ok()?;
        // f_type is signed on some architectures; compare the low 32 bits.
        #[allow(clippy::cast_sign_loss, clippy::unnecessary_cast)]
        let magic = (st.f_type as i64 as u64) & 0xFFFF_FFFF;
        match magic {
            NFS => Some("NFS"),
            SMB | CIFS | SMB2 => Some("SMB/CIFS"),
            FUSE => Some("FUSE (e.g. sshfs)"),
            _ => None,
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use std::path::Path;

    pub fn network_filesystem(_path: &Path) -> Option<&'static str> {
        None
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn second_lock_fails_with_exit_75() {
        let dir = tempfile::tempdir().unwrap();
        let first = DataDir::prepare(dir.path()).unwrap();
        let err = DataDir::prepare(dir.path()).unwrap_err();
        assert_eq!(err.code, 75);
        assert!(
            err.message.starts_with("another rackend instance is using"),
            "{}",
            err.message
        );
        first.release();
        DataDir::prepare(dir.path()).unwrap();
    }

    #[test]
    fn tmp_is_wiped_and_layout_created() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("tmp")).unwrap();
        fs::write(dir.path().join("tmp/stale-upload"), b"x").unwrap();
        let d = DataDir::prepare(dir.path()).unwrap();
        assert!(!d.root().join("tmp/stale-upload").exists());
        assert!(d.root().join("tmp").is_dir());
        assert!(d.backups().is_dir());
        assert!(d.root().join("rackend.lock").is_file());
    }
}
