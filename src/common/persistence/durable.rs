//! Durable append helpers shared by append-only stores.

use std::path::Path;

use tokio::fs;
use tokio::io::AsyncWriteExt;
use tracing::debug;

/// Append `bytes` with `O_APPEND`, flush the file, then best-effort sync
/// the containing directory.
///
/// The caller is responsible for serializing writers with [`super::FileLock`]
/// and any necessary in-process lock.
pub async fn append_bytes_durable(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }

    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    file.write_all(bytes).await?;
    file.sync_all().await?;
    drop(file);

    if let Some(parent) = path.parent() {
        sync_dir(parent).await;
    }
    Ok(())
}

async fn sync_dir(dir: &Path) {
    match fs::File::open(dir).await {
        Ok(file) => {
            if let Err(error) = file.sync_all().await {
                debug!(
                    path = %dir.display(),
                    %error,
                    "directory sync failed; continuing with best-effort durability"
                );
            }
        }
        Err(error) => {
            debug!(
                path = %dir.display(),
                %error,
                "directory could not be opened for sync"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn append_is_durable_and_preserves_existing_bytes() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("events.jsonl");

        append_bytes_durable(&path, b"one\n").await.unwrap();
        append_bytes_durable(&path, b"two\n").await.unwrap();

        assert_eq!(tokio::fs::read(&path).await.unwrap(), b"one\ntwo\n");
    }
}
