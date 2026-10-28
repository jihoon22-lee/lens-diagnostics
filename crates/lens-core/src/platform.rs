//! Native evidence primitives. Keep POSIX identity and nonblocking opens together:
//! a portable backend must preserve inode identity and FIFO-race protection.
#[cfg(not(unix))]
compile_error!("Lens currently requires a Unix identity backend; native Windows is not supported");

#[cfg(unix)]
pub(crate) fn file_identity(meta: &std::fs::Metadata) -> crate::FileIdentity {
    use std::os::unix::fs::MetadataExt;
    crate::FileIdentity {
        device: meta.dev(),
        inode: meta.ino(),
        mode: meta.mode(),
        size: meta.size(),
        modified_sec: meta.mtime(),
        modified_nsec: meta.mtime_nsec(),
        changed_sec: meta.ctime(),
        changed_nsec: meta.ctime_nsec(),
    }
}

#[cfg(unix)]
pub(crate) fn open_nonblocking(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
}
