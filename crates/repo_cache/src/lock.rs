//! Locks per repository copy: one held while writing to it, and one held shared while it's open.
//!
//! Two rozed processes, or two threads of one, can work on one repository at once, such as when
//! opening two pull requests of a stack. git locks single refs and files, but not a sequence of
//! commands (fetch, then pin; remove a checkout, then add it again), so rozed holds the write lock
//! for each such sequence. Every open [`crate::Repo`] holds the in-use lock shared, and cleanup
//! needs it exclusively, so nothing removes a clone or checkout a window still shows. Each is an
//! advisory `flock` on a file of its own outside the clone, so removing a clone never removes a
//! lock someone is waiting on.

use std::fs::{self, File, TryLockError};
use std::path::Path;

use crate::Error;

/// Held until dropped.
pub(crate) struct RepoLock {
    _file: File,
}

impl RepoLock {
    /// Waits for the lock at `path`, creating the file if needed.
    pub fn acquire(path: &Path) -> Result<Self, Error> {
        let file = open(path)?;
        file.lock()
            .map_err(Error::io(format!("locking {}", path.display())))?;
        Ok(Self { _file: file })
    }

    /// Waits for a shared hold on the lock at `path`, which any number can have at once and
    /// which keeps anyone from holding it alone.
    pub fn acquire_shared(path: &Path) -> Result<Self, Error> {
        let file = open(path)?;
        file.lock_shared()
            .map_err(Error::io(format!("locking {}", path.display())))?;
        Ok(Self { _file: file })
    }

    /// The lock at `path`, or `None` if someone else holds it, alone or shared.
    pub fn try_acquire(path: &Path) -> Result<Option<Self>, Error> {
        let file = open(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => {
                Err(Error::io(format!("locking {}", path.display()))(error))
            }
        }
    }
}

fn open(path: &Path) -> Result<File, Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(Error::io(format!("creating {}", parent.display())))?;
    }
    File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(Error::io(format!("opening {}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::mpsc;
    use std::time::Duration;

    use tempfile::TempDir;

    #[test]
    fn one_holder_at_a_time() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("locks/owner/repo.lock");

        let held = RepoLock::acquire(&path).unwrap();
        assert!(RepoLock::try_acquire(&path).unwrap().is_none());

        // A waiter gets it once the holder lets go.
        let (sender, receiver) = mpsc::channel();
        let waiter = std::thread::spawn({
            let path = path.clone();
            move || {
                let _lock = RepoLock::acquire(&path).unwrap();
                sender.send(()).unwrap();
            }
        });
        assert!(receiver.recv_timeout(Duration::from_millis(200)).is_err());
        drop(held);
        receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        waiter.join().unwrap();
        assert!(RepoLock::try_acquire(&path).unwrap().is_some());
    }

    #[test]
    fn shared_holders_keep_out_anyone_alone() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("locks/owner/repo.in-use");

        let first = RepoLock::acquire_shared(&path).unwrap();
        let second = RepoLock::acquire_shared(&path).unwrap();
        assert!(RepoLock::try_acquire(&path).unwrap().is_none());
        drop(first);
        assert!(RepoLock::try_acquire(&path).unwrap().is_none());
        drop(second);
        // A child process another test thread starts at that moment can hold a copy of the
        // lock's file descriptor until it runs its program, so allow a moment.
        let free = (0..100).any(|_| {
            let free = RepoLock::try_acquire(&path).unwrap().is_some();
            if !free {
                std::thread::sleep(Duration::from_millis(20));
            }
            free
        });
        assert!(free);
    }
}
