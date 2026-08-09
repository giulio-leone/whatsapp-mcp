#[cfg(unix)]
use anyhow::{Context, Result};
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
    PathBuf::from(format!("{db_path}.runtime.sock"))
}

#[cfg(unix)]
pub async fn claim_or_connect(path: &Path) -> Result<RuntimeEndpoint> {
    for attempt in 0..4 {
        match UnixStream::connect(path).await {
            Ok(stream) => return Ok(RuntimeEndpoint::Client(stream)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                if attempt == 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    continue;
                }
                let _ = std::fs::remove_file(path);
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("Connecting to WhatsApp runtime at {}", path.display())
                });
            }
        }

        match UnixListener::bind(path) {
            Ok(listener) => {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
                return Ok(RuntimeEndpoint::Owner(listener));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("Binding WhatsApp runtime at {}", path.display()));
            }
        }
    }

    let stream = UnixStream::connect(path)
        .await
        .with_context(|| format!("Connecting to WhatsApp runtime at {}", path.display()))?;
    Ok(RuntimeEndpoint::Client(stream))
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

    #[tokio::test]
    async fn second_process_connects_to_owned_runtime_socket() {
        let path = std::env::temp_dir().join(format!(
            "wa-mcp-runtime-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        let owner = claim_or_connect(&path).await.unwrap();
        assert!(matches!(owner, RuntimeEndpoint::Owner(_)));
        let client = claim_or_connect(&path).await.unwrap();
        assert!(matches!(client, RuntimeEndpoint::Client(_)));

        let _ = std::fs::remove_file(path);
    }
}
