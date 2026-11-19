use std::fs::{DirBuilder, Metadata, OpenOptions};
use std::path::Path;

pub(crate) trait Mode {
    fn mode(&mut self, mode: u32) -> &mut Self;
}

#[cfg(unix)]
impl Mode for OpenOptions {
    fn mode(&mut self, mode: u32) -> &mut Self {
        std::os::unix::fs::OpenOptionsExt::mode(self, mode)
    }
}

#[cfg(unix)]
impl Mode for DirBuilder {
    fn mode(&mut self, mode: u32) -> &mut Self {
        std::os::unix::fs::DirBuilderExt::mode(self, mode)
    }
}

#[cfg(windows)]
impl Mode for OpenOptions {
    fn mode(&mut self, _mode: u32) -> &mut Self {
        self
    }
}

#[cfg(windows)]
impl Mode for DirBuilder {
    fn mode(&mut self, _mode: u32) -> &mut Self {
        self
    }
}

#[cfg(unix)]
pub(crate) fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(windows)]
pub(crate) fn set_mode(path: &Path, _mode: u32) -> std::io::Result<()> {
    std::fs::metadata(path).map(|_| ())
}

#[cfg(unix)]
#[allow(clippy::unnecessary_wraps)]
pub(crate) fn link_count(metadata: &Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.nlink())
}

#[cfg(windows)]
pub(crate) fn link_count(_metadata: &Metadata) -> Option<u64> {
    None
}

#[cfg(unix)]
pub(crate) fn is_executable(metadata: &Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(windows)]
pub(crate) fn is_executable(_metadata: &Metadata) -> bool {
    false
}

#[cfg(unix)]
pub(crate) fn symlink(link: &Path, at: &Path, _to_dir: bool) -> std::io::Result<()> {
    std::os::unix::fs::symlink(link, at)
}

#[cfg(windows)]
pub(crate) fn symlink(link: &Path, at: &Path, to_dir: bool) -> std::io::Result<()> {
    if to_dir {
        std::os::windows::fs::symlink_dir(link, at)
    } else {
        std::os::windows::fs::symlink_file(link, at)
    }
}
