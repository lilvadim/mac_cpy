use crate::progress;
use std::{fs::File, io, path::Path};

pub(crate) fn sync(file: &File, path: &Path) -> io::Result<()> {
    handle_sync(file.sync_all(), path)
}

fn handle_sync(result: io::Result<()>, path: &Path) -> io::Result<()> {
    match result {
        Err(e)
            if e.kind() == io::ErrorKind::Unsupported
                || e.raw_os_error().is_some_and(|code| {
                    code == libc::ENOTSUP || code == libc::EOPNOTSUPP || code == libc::ENOSYS
                }) =>
        {
            progress::log(&format!(
                "WARNING: Cannot force storage synchronization for {}: {e}. Continuing; durable storage is not confirmed.",
                path.display()
            ));
            Ok(())
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_sync_is_nonfatal_but_write_errors_are_fatal() {
        let path = Path::new("/test/archive.partial");
        for code in [libc::ENOTSUP, libc::EOPNOTSUPP, libc::ENOSYS] {
            assert!(handle_sync(Err(io::Error::from_raw_os_error(code)), path).is_ok());
        }
        for code in [libc::EIO, libc::ENOSPC, libc::EACCES, libc::EBADF] {
            assert_eq!(
                handle_sync(Err(io::Error::from_raw_os_error(code)), path)
                    .unwrap_err()
                    .raw_os_error(),
                Some(code)
            );
        }
    }
}
