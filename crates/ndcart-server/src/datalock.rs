//! One writer at a time for a data folder. `ndcart serve` keeps an advisory lock on
//! `<data>/.ndcart.lock` for as long as it runs; a command that writes projects without the
//! server (`ndcart draft --accept-into`) takes the same lock first and refuses when the studio
//! holds it, so the two never write one project at once. The operating system drops the lock
//! when the process ends, however it ends, so a crash leaves nothing stale.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;

const NAME: &str = ".ndcart.lock";

/// Holds the lock until dropped.
#[derive(Debug)]
pub struct Held {
    _file: File,
}

#[derive(Debug)]
pub enum Taken {
    Held(Held),
    /// Another process holds it.
    Busy,
    /// The folder or file could not be used (the reason).
    Unavailable(String),
}

/// Try to take the folder's lock (creating the folder and the lock file when needed).
pub fn take(dir: &Path) -> Taken {
    if let Err(e) = std::fs::create_dir_all(dir) {
        return Taken::Unavailable(format!("cannot create {}: {e}", dir.display()));
    }
    let file = match OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(NAME))
    {
        Ok(f) => f,
        Err(e) => return Taken::Unavailable(format!("cannot open {NAME}: {e}")),
    };
    match file.try_lock() {
        Ok(()) => Taken::Held(Held { _file: file }),
        Err(TryLockError::WouldBlock) => Taken::Busy,
        Err(TryLockError::Error(e)) => Taken::Unavailable(format!("cannot lock {NAME}: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_second_taker_is_told_it_is_busy_until_the_first_lets_go() {
        let dir = std::env::temp_dir().join(format!("ndcart-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let first = take(&dir);
        assert!(matches!(first, Taken::Held(_)), "{first:?}");
        assert!(matches!(take(&dir), Taken::Busy));
        drop(first);
        assert!(matches!(take(&dir), Taken::Held(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
