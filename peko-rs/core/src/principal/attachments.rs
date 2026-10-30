//! Attachments in direct messages (ADR-088).
//!
//! The local operator sends files by path. The runtime copies each one
//! into the principal's workspace (`attachments/<YYYY-MM-DD>/<8 hex>-<name>`),
//! records it on the inbound DM post, and tells the turn about it with
//! one note line per file. Images additionally reach a vision model as
//! image blocks (`agents::subagent_executor::ImageInput`).

use std::path::{Path, PathBuf};

use peko_protocol::channel::Attachment;

use crate::agents::subagent_executor::{ImageInput, MAX_IMAGE_BLOCK_BYTES};

/// At most this many files per message.
pub(crate) const MAX_FILES: usize = 8;
/// At most this many bytes per file.
pub(crate) const MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;
/// Workspace directory the copies live under.
pub(crate) const DIR: &str = "attachments";

/// Validate every path, then copy each file into the workspace. Nothing
/// is copied unless every file passes, so a refused request leaves no
/// trace. The error is the user-facing reason.
pub(crate) async fn ingest(workspace: &Path, paths: &[String]) -> Result<Vec<Attachment>, String> {
    if paths.len() > MAX_FILES {
        return Err(format!(
            "too many attachments ({}); at most {MAX_FILES} per message",
            paths.len()
        ));
    }
    let mut sources = Vec::with_capacity(paths.len());
    for raw in paths {
        let path = PathBuf::from(raw);
        if !path.is_absolute() {
            return Err(format!("attachment path must be absolute: {raw}"));
        }
        let meta = tokio::fs::metadata(&path)
            .await
            .map_err(|e| format!("cannot read attachment {raw}: {e}"))?;
        if !meta.is_file() {
            return Err(format!("attachment is not a regular file: {raw}"));
        }
        if meta.len() > MAX_FILE_BYTES {
            return Err(format!(
                "attachment {raw} is {}; the limit is {}",
                human_size(meta.len()),
                human_size(MAX_FILE_BYTES)
            ));
        }
        sources.push(path);
    }

    let day = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let dir = Path::new(DIR).join(&day);
    tokio::fs::create_dir_all(workspace.join(&dir))
        .await
        .map_err(|e| format!("cannot create the attachments directory: {e}"))?;
    let mut attachments = Vec::with_capacity(sources.len());
    for source in sources {
        let name = source
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_string());
        let id = &uuid::Uuid::new_v4().simple().to_string()[..8];
        let relative = dir.join(format!("{id}-{}", safe_file_name(&name)));
        let dest = workspace.join(&relative);
        let size = tokio::fs::copy(&source, &dest)
            .await
            .map_err(|e| format!("cannot copy attachment {}: {e}", source.display()))?;
        let mime = mime_of(&dest).await;
        attachments.push(Attachment {
            name,
            path: relative.to_string_lossy().replace('\\', "/"),
            mime,
            size,
        });
    }
    Ok(attachments)
}

/// The text a turn sees: the message, then one note per attachment
/// (ADR-088 D4).
pub(crate) fn turn_text(message: &str, workspace: &Path, attachments: &[Attachment]) -> String {
    if attachments.is_empty() {
        return message.to_string();
    }
    let notes = attachments
        .iter()
        .map(|a| {
            format!(
                "[attached: {} ({}, {}) at {}]",
                a.name,
                a.mime,
                human_size(a.size),
                absolute(workspace, a).display()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    if message.trim().is_empty() {
        notes
    } else {
        format!("{message}\n\n{notes}")
    }
}

/// The attachments a vision model may see as image blocks.
pub(crate) fn images(workspace: &Path, attachments: &[Attachment]) -> Vec<ImageInput> {
    attachments
        .iter()
        .filter(|a| is_viewable_image(&a.mime) && a.size <= MAX_IMAGE_BLOCK_BYTES)
        .map(|a| ImageInput {
            path: absolute(workspace, a),
            mime: a.mime.clone(),
        })
        .collect()
}

/// An attachment's absolute path on this host.
pub(crate) fn absolute(workspace: &Path, attachment: &Attachment) -> PathBuf {
    workspace.join(&attachment.path)
}

fn is_viewable_image(mime: &str) -> bool {
    matches!(
        mime,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    )
}

/// Keep letters, digits, `.`, `-`, and `_`; everything else becomes `_`.
fn safe_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim_start_matches('.');
    let cleaned: String = cleaned
        .chars()
        .rev()
        .take(100)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if cleaned.is_empty() {
        "file".to_string()
    } else {
        cleaned
    }
}

/// Images by their leading bytes, everything else by extension.
async fn mime_of(path: &Path) -> String {
    use tokio::io::AsyncReadExt;
    let mut head = [0u8; 12];
    let read = match tokio::fs::File::open(path).await {
        Ok(mut file) => file.read(&mut head).await.unwrap_or(0),
        Err(_) => 0,
    };
    let head = &head[..read];
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        return "image/png".into();
    }
    if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return "image/jpeg".into();
    }
    if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        return "image/gif".into();
    }
    if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        return "image/webp".into();
    }
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "txt" | "log" => "text/plain",
        "md" | "markdown" => "text/markdown",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        "json" => "application/json",
        "xml" => "application/xml",
        "zip" => "application/zip",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "svg" => "image/svg+xml",
        "heic" => "image/heic",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        _ => "application/octet-stream",
    }
    .into()
}

fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{:.1} MB", b / (KB * KB))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    #[tokio::test]
    async fn ingest_copies_into_the_workspace_and_sniffs_images() {
        let src = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let photo = src.path().join("my photo.jpg"); // PNG bytes, wrong extension
        std::fs::write(&photo, PNG).unwrap();
        let notes = src.path().join("notes.md");
        std::fs::write(&notes, "# hi").unwrap();

        let got = ingest(
            workspace.path(),
            &[photo.display().to_string(), notes.display().to_string()],
        )
        .await
        .unwrap();

        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "my photo.jpg");
        assert_eq!(got[0].mime, "image/png");
        assert_eq!(got[0].size, PNG.len() as u64);
        assert!(got[0].path.starts_with("attachments/"));
        assert!(got[0].path.ends_with("-my_photo.jpg"));
        assert_eq!(
            std::fs::read(workspace.path().join(&got[0].path)).unwrap(),
            PNG
        );
        assert_eq!(got[1].mime, "text/markdown");

        let images = images(workspace.path(), &got);
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].mime, "image/png");
    }

    #[tokio::test]
    async fn ingest_refuses_before_copying_anything() {
        let src = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let ok = src.path().join("a.txt");
        std::fs::write(&ok, "a").unwrap();

        let relative = ingest(workspace.path(), &["a.txt".into()]).await;
        assert!(relative.unwrap_err().contains("absolute"));

        let dir = ingest(
            workspace.path(),
            &[ok.display().to_string(), src.path().display().to_string()],
        )
        .await;
        assert!(dir.unwrap_err().contains("not a regular file"));

        let missing = ingest(workspace.path(), &["/no/such/file.png".into()]).await;
        assert!(missing.unwrap_err().contains("cannot read"));

        let many = vec![ok.display().to_string(); MAX_FILES + 1];
        assert!(ingest(workspace.path(), &many)
            .await
            .unwrap_err()
            .contains("too many"));

        assert!(!workspace.path().join(DIR).exists());
    }

    #[test]
    fn turn_text_adds_one_note_per_file() {
        let workspace = Path::new("/w");
        let a = Attachment {
            name: "photo.png".into(),
            path: "attachments/2026-10-30/ab12cd34-photo.png".into(),
            mime: "image/png".into(),
            size: 1_258_291,
        };
        assert_eq!(
            turn_text("look", workspace, std::slice::from_ref(&a)),
            "look\n\n[attached: photo.png (image/png, 1.2 MB) at /w/attachments/2026-10-30/ab12cd34-photo.png]"
        );
        assert!(turn_text("", workspace, &[a]).starts_with("[attached: "));
        assert_eq!(turn_text("plain", workspace, &[]), "plain");
    }

    #[test]
    fn file_names_are_made_safe() {
        assert_eq!(safe_file_name("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(
            safe_file_name("Q3 report (final).pdf"),
            "Q3_report__final_.pdf"
        );
        assert_eq!(safe_file_name(".env"), "env");
        assert_eq!(safe_file_name(""), "file");
    }
}
