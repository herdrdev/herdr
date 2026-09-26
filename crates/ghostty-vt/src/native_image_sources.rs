//! Bounded kernel CoW snapshots. No read/write or whole-file clone fallback.
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;

#[repr(C)]
struct CloneRange {
    src_fd: i64,
    src_offset: u64,
    src_length: u64,
    dest_offset: u64,
}

fn stat(fd: libc::c_int) -> io::Result<libc::stat> {
    let mut result = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fstat initializes the supplied stat on success; fd remains borrowed.
    if unsafe { libc::fstat(fd, result.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful fstat initialized the value.
    Ok(unsafe { result.assume_init() })
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid native snapshot source or destination",
    )
}

pub(crate) fn clone_native_image_source(
    source_fd: i64,
    destination: &File,
    expected_len: usize,
) -> io::Result<()> {
    let fd = libc::c_int::try_from(source_fd).map_err(|_| invalid())?;
    if fd < 0
        || expected_len == 0
        || expected_len > 16 * 1024 * 1024
        || !expected_len.is_multiple_of(4)
    {
        return Err(invalid());
    }
    let source = stat(fd)?;
    let dest = stat(destination.as_raw_fd())?;
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    let valid_source = |value: &libc::stat| {
        value.st_mode & libc::S_IFMT == libc::S_IFREG
            && value.st_uid == uid
            && value.st_size == expected_len as i64
            && value.st_dev == source.st_dev
            && value.st_ino == source.st_ino
    };
    if !valid_source(&source)
        || dest.st_mode & libc::S_IFMT != libc::S_IFREG
        || dest.st_mode & 0o777 != 0o600
        || dest.st_uid != uid
        || dest.st_nlink != 1
        || dest.st_size != 0
        || (dest.st_dev == source.st_dev && dest.st_ino == source.st_ino)
    {
        return Err(invalid());
    }
    let range = CloneRange {
        src_fd: source_fd,
        src_offset: 0,
        src_length: expected_len as u64,
        dest_offset: 0,
    };
    // Explicit nonzero length is essential: FICLONE (or a zero range length)
    // clones through EOF and source growth could exceed the reservation before
    // post-validation. This request cannot extend the destination past the
    // reservation even if the producer changes size concurrently. Filesystems
    // requiring block alignment may reject an EOF range after concurrent growth;
    // that is a normal fallback, never a reason to try an unbounded clone.
    // SAFETY: the request consumes a valid repr(C) range, both descriptors stay
    // open throughout the call, and the source descriptor is never owned here.
    if unsafe { libc::ioctl(destination.as_raw_fd(), libc::FICLONERANGE, &range) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let after = stat(destination.as_raw_fd())?;
    if !valid_source(&stat(fd)?)
        || after.st_dev != dest.st_dev
        || after.st_ino != dest.st_ino
        || after.st_size != expected_len as i64
        || after.st_nlink != 1
        || after.st_uid != uid
        || after.st_mode & 0o777 != 0o600
    {
        return Err(invalid());
    }
    // The kernel serializes clone against writes. These stats establish exact
    // length at validation points, not producer quiescence or a content hash.
    Ok(())
}
