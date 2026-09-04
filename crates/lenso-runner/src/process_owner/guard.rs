use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

use fs2::FileExt;
use sha2::{Digest, Sha256};

/// Lock files are outside the replaceable project root and are never unlinked.
#[derive(Debug)]
pub struct RootGuard(Vec<File>);

impl RootGuard {
    pub fn acquire(root: &Path, registry: &Path) -> io::Result<Self> {
        let root = fs::canonicalize(root)?;
        if !root.is_dir() || !registry.is_absolute() {
            return Err(io::Error::other(
                "root must be a directory and ownership registry must be absolute",
            ));
        }
        fs::create_dir_all(registry)?;
        let registry = fs::canonicalize(registry)?;
        if registry.starts_with(&root) {
            return Err(io::Error::other(
                "ownership registry must be outside the replaceable root",
            ));
        }
        let metadata = fs::metadata(&root)?;
        let keys = [
            format!(
                "path-{:x}",
                Sha256::digest(root.as_os_str().as_encoded_bytes())
            ),
            format!("inode-{}-{}", metadata.dev(), metadata.ino()),
        ];
        let mut files = Vec::new();
        for key in keys {
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(registry.join(format!("{key}.lock")))?;
            file.try_lock_exclusive()
                .map_err(|_| io::Error::other("application root already owned"))?;
            if file.metadata()?.len() > 1024 {
                return Err(io::Error::other("invalid ownership record"));
            }
            let mut record = String::new();
            file.read_to_string(&mut record)?;
            if !record.is_empty() && record != "settled\n" {
                return Err(io::Error::other(
                    "previous execution is unconfirmed; settle native ownership before recovery",
                ));
            }
            files.push(file);
        }
        // Record uncertainty before spawn. A helper crash cannot silently release
        // recovery admission merely because the OS releases its advisory lock.
        for file in &mut files {
            write_record(file, b"unconfirmed\n")?;
        }
        File::open(registry)?.sync_all()?;
        Ok(Self(files))
    }

    pub fn settled(&mut self) -> io::Result<()> {
        for file in &mut self.0 {
            write_record(file, b"settled\n")?;
        }
        Ok(())
    }
}

fn write_record(file: &mut File, bytes: &[u8]) -> io::Result<()> {
    file.seek(SeekFrom::Start(0))?;
    file.write_all(bytes)?;
    file.set_len(bytes.len() as u64)?;
    file.sync_all()
}
