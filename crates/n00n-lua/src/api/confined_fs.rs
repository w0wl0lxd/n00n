use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{Error, ErrorKind, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Component, Path};
use std::sync::atomic::{AtomicU64, Ordering};

use rustix::fs::{
    AtFlags, Dir, FileType, Mode, OFlags, Stat, fchmod, fsync, open, openat, renameat, statat,
    unlinkat,
};

const NANOSECONDS_PER_SECOND: i64 = 1_000_000_000;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(crate) struct Metadata {
    pub(crate) size: u64,
    pub(crate) is_file: bool,
    pub(crate) is_dir: bool,
    pub(crate) is_symlink: bool,
    pub(crate) mtime: i64,
}

pub(crate) struct DirEntry {
    pub(crate) name: String,
    pub(crate) kind: &'static str,
}

fn invalid_path() -> Error {
    Error::new(
        ErrorKind::PermissionDenied,
        "path traversal outside base directory is not allowed",
    )
}

fn confined_open_error(error: rustix::io::Errno) -> Error {
    match error {
        rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR => Error::new(
            ErrorKind::PermissionDenied,
            "symbolic links are not allowed in confined paths",
        ),
        error => Error::from(error),
    }
}

fn components(path: &Path) -> std::io::Result<Vec<OsString>> {
    let components: Vec<_> = path
        .components()
        .map(|component| match component {
            Component::Normal(name) => Ok(name.to_os_string()),
            _ => Err(invalid_path()),
        })
        .collect::<Result<_, _>>()?;
    if components.is_empty() {
        return Err(invalid_path());
    }
    Ok(components)
}

fn open_base(base: &Path) -> std::io::Result<OwnedFd> {
    open(
        base,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(Error::from)
}

fn open_parent(base: &Path, relative: &Path) -> std::io::Result<(OwnedFd, OsString)> {
    let mut components = components(relative)?;
    let name = components.pop().ok_or_else(invalid_path)?;
    let mut directory = open_base(base)?;
    for component in components {
        directory = openat(
            &directory,
            component.as_os_str(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(confined_open_error)?;
    }
    Ok((directory, name))
}

pub(crate) fn read(base: &Path, relative: &Path) -> std::io::Result<String> {
    let (parent, name) = open_parent(base, relative)?;
    let fd = openat(
        &parent,
        name.as_os_str(),
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(confined_open_error)?;
    let mut file = File::from(fd);
    if !file.metadata()?.is_file() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "path is not a regular file",
        ));
    }
    let mut content = String::new();
    file.read_to_string(&mut content)?;
    Ok(content)
}

pub(crate) fn metadata(base: &Path, relative: &Path) -> std::io::Result<Option<Metadata>> {
    let (parent, name) = open_parent(base, relative)?;
    let stat = match statat(&parent, name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(error) => return Err(Error::from(error)),
    };
    metadata_from_stat(&stat).map(Some)
}

pub(crate) fn dir(base: &Path) -> std::io::Result<Vec<DirEntry>> {
    let directory = open_base(base)?;
    let iterator = Dir::new(directory).map_err(Error::from)?;
    let mut entries = Vec::new();
    for entry in iterator {
        let entry = entry.map_err(Error::from)?;
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        let name = OsStr::from_bytes(name)
            .to_str()
            .ok_or_else(|| Error::new(ErrorKind::InvalidData, "non-utf8 path"))?
            .to_owned();
        let kind = match entry.file_type() {
            FileType::RegularFile => "file",
            FileType::Directory => "directory",
            FileType::Symlink => "link",
            _ => "unknown",
        };
        entries.push(DirEntry { name, kind });
    }
    Ok(entries)
}

pub(crate) fn write(base: &Path, relative: &Path, content: &[u8]) -> std::io::Result<()> {
    let (parent, name) = open_parent(base, relative)?;
    let existing = match statat(&parent, name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => Some(stat),
        Err(rustix::io::Errno::NOENT) => None,
        Err(error) => return Err(Error::from(error)),
    };
    if existing
        .as_ref()
        .is_some_and(|stat| FileType::from_raw_mode(stat.st_mode).is_symlink())
    {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "destination must not be a symbolic link",
        ));
    }
    let temporary_name = loop {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = format!(".n00n-tmp-{}-{sequence}", std::process::id());
        match openat(
            &parent,
            candidate.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        ) {
            Ok(fd) => break (candidate, fd),
            Err(rustix::io::Errno::EXIST) => {}
            Err(error) => return Err(Error::from(error)),
        }
    };
    let (temporary_name, fd) = temporary_name;
    let mut temporary = File::from(fd);
    let result = (|| {
        if let Some(stat) = existing {
            fchmod(&temporary, Mode::from_raw_mode(stat.st_mode)).map_err(Error::from)?;
        }
        temporary.write_all(content)?;
        temporary.sync_all()?;
        renameat(&parent, temporary_name.as_str(), &parent, name.as_os_str())
            .map_err(Error::from)?;
        fsync(&parent).map_err(Error::from)
    })();
    if result.is_err() {
        let _ = unlinkat(&parent, temporary_name.as_str(), AtFlags::empty());
    }
    result
}

fn metadata_from_stat(stat: &Stat) -> std::io::Result<Metadata> {
    let file_type = FileType::from_raw_mode(stat.st_mode);
    #[allow(clippy::useless_conversion)]
    let nanoseconds = i64::try_from(stat.st_mtime_nsec)
        .map_err(|_| Error::other("modification time is out of range"))?;
    let mtime = stat
        .st_mtime
        .checked_mul(NANOSECONDS_PER_SECOND)
        .and_then(|seconds| seconds.checked_add(nanoseconds))
        .ok_or_else(|| Error::other("modification time is out of range"))?;
    Ok(Metadata {
        size: stat
            .st_size
            .try_into()
            .map_err(|_| Error::other("negative file size"))?,
        is_file: file_type.is_file(),
        is_dir: file_type.is_dir(),
        is_symlink: file_type.is_symlink(),
        mtime,
    })
}

fn metadata_at(parent: &OwnedFd, name: &OsStr) -> std::io::Result<Option<Metadata>> {
    let stat = match statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(error) => return Err(Error::from(error)),
    };
    metadata_from_stat(&stat).map(Some)
}

pub(crate) fn remove(base: &Path, relative: &Path) -> std::io::Result<()> {
    let (parent, name) = open_parent(base, relative)?;
    let metadata = metadata_at(&parent, name.as_os_str())?
        .ok_or_else(|| Error::new(ErrorKind::NotFound, "path does not exist"))?;
    let flags = if metadata.is_dir {
        AtFlags::REMOVEDIR
    } else {
        AtFlags::empty()
    };
    unlinkat(&parent, name.as_os_str(), flags).map_err(Error::from)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn metadata_reports_nanoseconds_since_epoch() {
        let base = TempDir::new().unwrap();
        let path = base.path().join("timestamped.md");
        fs::write(&path, "content").unwrap();
        let expected = fs::metadata(&path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();

        let actual = metadata(base.path(), Path::new("timestamped.md"))
            .unwrap()
            .unwrap()
            .mtime;

        assert_eq!(u128::try_from(actual).unwrap(), expected);
    }

    #[test]
    fn write_preserves_existing_permissions() {
        let base = TempDir::new().unwrap();
        let path = base.path().join("existing.md");
        fs::write(&path, "old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();

        write(base.path(), Path::new("existing.md"), b"new").unwrap();

        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    #[test]
    fn remove_unlinks_symlink_instead_of_target() {
        let base = TempDir::new().unwrap();
        fs::write(base.path().join("target.md"), "target").unwrap();
        symlink("target.md", base.path().join("link.md")).unwrap();

        remove(base.path(), Path::new("link.md")).unwrap();

        assert_eq!(
            fs::read_to_string(base.path().join("target.md")).unwrap(),
            "target"
        );
        assert!(!base.path().join("link.md").exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn read_rejects_fifo_without_blocking() {
        const RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

        let base = TempDir::new().unwrap();
        rustix::fs::mknodat(
            rustix::fs::CWD,
            base.path().join("pipe"),
            FileType::Fifo,
            Mode::RUSR | Mode::WUSR,
            rustix::fs::makedev(0, 0),
        )
        .unwrap();
        let path = base.path().to_owned();
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let result = read(&path, Path::new("pipe")).map_err(|error| error.kind());
            sender.send(result).unwrap();
        });

        assert_eq!(
            receiver
                .recv_timeout(RESPONSE_TIMEOUT)
                .unwrap()
                .unwrap_err(),
            ErrorKind::InvalidInput
        );
        worker.join().unwrap();
    }

    #[test]
    fn operations_reject_symlinked_parent() {
        let base = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        symlink(outside.path(), base.path().join("escape")).unwrap();

        let error = write(base.path(), Path::new("escape/new.md"), b"escaped").unwrap_err();

        assert_eq!(error.kind(), ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("symbolic links are not allowed"));
        assert!(!outside.path().join("new.md").exists());
    }
}
