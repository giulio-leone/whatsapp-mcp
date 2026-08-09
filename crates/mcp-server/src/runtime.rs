#[cfg(unix)]
use anyhow::{Context, Result};
#[cfg(unix)]
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::fs::{File, OpenOptions};
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
#[cfg(unix)]
use std::path::{Path, PathBuf};
#[cfg(unix)]
use tokio::io::{copy, AsyncWriteExt};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};

#[cfg(unix)]
pub enum RuntimeEndpoint {
    Owner(UnixListener),
    Client(UnixStream),
}

#[cfg(unix)]
pub fn socket_path(db_path: &str) -> PathBuf {
    let adjacent_path = PathBuf::from(format!("{db_path}.runtime.sock"));
    if adjacent_path.as_os_str().as_bytes().len() < 100 {
        return adjacent_path;
    }

    let db_identity = if Path::new(db_path).is_absolute() {
        PathBuf::from(db_path)
    } else {
        std::env::current_dir().unwrap_or_default().join(db_path)
    };
    let digest = Sha256::digest(db_identity.as_os_str().as_bytes());
    let key = digest[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    runtime_directory().join(format!("{key}.sock"))
}

#[cfg(unix)]
const LOCK_EX: i32 = 2;

#[cfg(unix)]
unsafe extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
    fn geteuid() -> u32;
}

#[cfg(unix)]
fn runtime_directory() -> PathBuf {
    PathBuf::from(format!("/tmp/wa-mcp-{}", unsafe { geteuid() }))
}

#[cfg(unix)]
fn prepare_socket_parent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let private_runtime_directory = parent == runtime_directory();
    if private_runtime_directory {
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700);
        match builder.create(parent) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "Creating WhatsApp runtime directory at {}",
                        parent.display()
                    )
                });
            }
        }
    }
    let metadata = std::fs::symlink_metadata(parent).with_context(|| {
        format!(
            "Inspecting WhatsApp runtime directory at {}",
            parent.display()
        )
    })?;
    if !metadata.file_type().is_dir()
        || metadata.uid() != unsafe { geteuid() }
        || metadata.mode() & 0o022 != 0
    {
        anyhow::bail!(
            "WhatsApp runtime directory must be an owner-controlled, non-writable-by-others directory: {}",
            parent.display()
        );
    }
    if private_runtime_directory {
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(unix)]
fn owned_socket_metadata(path: &Path) -> Result<Option<std::fs::Metadata>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!("Inspecting WhatsApp runtime socket at {}", path.display())
            });
        }
    };
    if !metadata.file_type().is_socket() || metadata.uid() != unsafe { geteuid() } {
        anyhow::bail!(
            "WhatsApp runtime endpoint is not an owned Unix socket: {}",
            path.display()
        );
    }
    Ok(Some(metadata))
}

#[cfg(unix)]
fn ensure_private_socket(path: &Path) -> Result<()> {
    let metadata = owned_socket_metadata(path)?
        .with_context(|| format!("WhatsApp runtime socket disappeared: {}", path.display()))?;
    if metadata.mode() & 0o077 != 0 {
        anyhow::bail!(
            "WhatsApp runtime socket is accessible by another user or group: {}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(unix)]
fn election_lock_path(socket_path: &Path) -> PathBuf {
    let mut path = socket_path.as_os_str().to_os_string();
    path.push(".owner.lock");
    PathBuf::from(path)
}

#[cfg(unix)]
struct ElectionLock {
    #[allow(dead_code)]
    file: File,
}

#[cfg(unix)]
impl ElectionLock {
    fn acquire(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .with_context(|| format!("Opening runtime election lock at {}", path.display()))?;
        let metadata = file.metadata()?;
        if !metadata.file_type().is_file() || metadata.uid() != unsafe { geteuid() } {
            anyhow::bail!(
                "WhatsApp runtime election lock is not an owned regular file: {}",
                path.display()
            );
        }
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        let result = unsafe { flock(file.as_raw_fd(), LOCK_EX) };
        if result != 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("Locking runtime election at {}", path.display()));
        }
        Ok(Self { file })
    }
}

#[cfg(unix)]
pub async fn claim_or_connect(path: &Path) -> Result<RuntimeEndpoint> {
    prepare_socket_parent(path)?;
    owned_socket_metadata(path)?;
    match UnixStream::connect(path).await {
        Ok(stream) => {
            ensure_private_socket(path)?;
            return Ok(RuntimeEndpoint::Client(stream));
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) => {}
        Err(error) => {
            return Err(error)
                .with_context(|| format!("Connecting to WhatsApp runtime at {}", path.display()));
        }
    }

    let lock_path = election_lock_path(path);
    let _election = tokio::task::spawn_blocking(move || ElectionLock::acquire(&lock_path))
        .await
        .context("Joining WhatsApp runtime election")??;

    owned_socket_metadata(path)?;
    match UnixStream::connect(path).await {
        Ok(stream) => {
            ensure_private_socket(path)?;
            return Ok(RuntimeEndpoint::Client(stream));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
            if owned_socket_metadata(path)?.is_some() {
                std::fs::remove_file(path).with_context(|| {
                    format!(
                        "Removing stale WhatsApp runtime socket at {}",
                        path.display()
                    )
                })?;
            }
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("Rechecking WhatsApp runtime at {}", path.display()));
        }
    }

    let staging_path = path.with_extension("tmp");
    if owned_socket_metadata(&staging_path)?.is_some() {
        std::fs::remove_file(&staging_path).with_context(|| {
            format!(
                "Removing stale staged WhatsApp runtime socket at {}",
                staging_path.display()
            )
        })?;
    }
    let listener = UnixListener::bind(&staging_path).with_context(|| {
        format!(
            "Binding staged WhatsApp runtime at {}",
            staging_path.display()
        )
    })?;
    if let Err(error) =
        std::fs::set_permissions(&staging_path, std::fs::Permissions::from_mode(0o600))
    {
        let _ = std::fs::remove_file(&staging_path);
        return Err(error).with_context(|| {
            format!(
                "Protecting staged WhatsApp runtime at {}",
                staging_path.display()
            )
        });
    }
    if let Err(error) = std::fs::rename(&staging_path, path) {
        let _ = std::fs::remove_file(&staging_path);
        return Err(error)
            .with_context(|| format!("Publishing WhatsApp runtime at {}", path.display()));
    }
    Ok(RuntimeEndpoint::Owner(listener))
}

#[cfg(unix)]
pub async fn proxy_stdio(stream: UnixStream) -> Result<()> {
    let (mut runtime_reader, mut runtime_writer) = stream.into_split();
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();

    let request_side = async {
        copy(&mut stdin, &mut runtime_writer).await?;
        runtime_writer.shutdown().await
    };
    let response_side = async {
        copy(&mut runtime_reader, &mut stdout).await?;
        stdout.flush().await
    };

    tokio::try_join!(request_side, response_side)?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn socket_path_is_stable_short_and_user_private() {
        let db_path = format!("/{}/whatsapp.db", "very-long-directory/".repeat(20));
        let first = socket_path(&db_path);
        assert_eq!(first, socket_path(&db_path));
        assert_eq!(first.parent(), Some(runtime_directory().as_path()));
        assert!(first.as_os_str().as_bytes().len() < 100);
    }

    #[test]
    fn socket_path_preserves_compatible_adjacent_runtime_address() {
        let db_path = "/tmp/wa-mcp-compatible.db";
        assert_eq!(
            socket_path(db_path),
            PathBuf::from(format!("{db_path}.runtime.sock"))
        );
    }

    #[test]
    fn relative_socket_path_validates_the_current_directory() {
        let path = socket_path("whatsapp.db");
        assert_eq!(path, PathBuf::from("whatsapp.db.runtime.sock"));
        prepare_socket_parent(&path).unwrap();
    }

    #[tokio::test]
    async fn concurrent_claimants_replace_one_stale_socket_with_exactly_one_owner() {
        let path = socket_path(&format!(
            "/tmp/{}/wa-mcp-runtime-{}-{}.db",
            "long-runtime-directory/".repeat(8),
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        prepare_socket_parent(&path).unwrap();
        let stale = std::os::unix::net::UnixListener::bind(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        drop(stale);

        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(8));
        let mut claims = Vec::new();
        for _ in 0..8 {
            let barrier = barrier.clone();
            let path = path.clone();
            claims.push(tokio::spawn(async move {
                barrier.wait().await;
                claim_or_connect(&path).await.unwrap()
            }));
        }
        let mut endpoints = Vec::new();
        for claim in claims {
            endpoints.push(claim.await.unwrap());
        }

        assert_eq!(
            endpoints
                .iter()
                .filter(|endpoint| matches!(endpoint, RuntimeEndpoint::Owner(_)))
                .count(),
            1
        );
        assert_eq!(
            endpoints
                .iter()
                .filter(|endpoint| matches!(endpoint, RuntimeEndpoint::Client(_)))
                .count(),
            7
        );

        drop(endpoints);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(election_lock_path(&path));
    }

    #[tokio::test]
    async fn rejects_non_socket_endpoint_and_symlinked_election_lock() {
        use std::os::unix::fs::symlink;

        let directory = std::env::temp_dir().join(format!(
            "wa-mcp-security-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700).create(&directory).unwrap();
        let endpoint = directory.join("runtime.sock");
        File::create(&endpoint).unwrap();
        assert!(claim_or_connect(&endpoint).await.is_err());

        std::fs::remove_file(&endpoint).unwrap();
        let target = directory.join("target");
        File::create(&target).unwrap();
        let lock = directory.join("runtime.lock");
        symlink(&target, &lock).unwrap();
        assert!(ElectionLock::acquire(&lock).is_err());

        let _ = std::fs::remove_file(&lock);
        let _ = std::fs::remove_file(&target);
        let _ = std::fs::remove_dir(&directory);
    }
}
