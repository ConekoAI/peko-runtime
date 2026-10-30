//! A file's MIME type, for attachments and for `Read` on media files
//! (ADR-088): images, WAV, FLAC, OGG, MP3 (ID3), and AVI by their leading
//! bytes, everything else by extension.

use std::path::Path;

/// Images by their leading bytes, everything else by extension.
pub(crate) async fn mime_of(path: &Path) -> String {
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
    if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WAVE" {
        return "audio/wav".into();
    }
    if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"AVI " {
        return "video/x-msvideo".into();
    }
    if head.starts_with(b"fLaC") {
        return "audio/flac".into();
    }
    if head.starts_with(b"OggS") {
        return "audio/ogg".into();
    }
    if head.starts_with(b"ID3") {
        return "audio/mpeg".into();
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
        "flac" => "audio/flac",
        "ogg" | "oga" => "audio/ogg",
        "m4a" => "audio/mp4",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "avi" => "video/x-msvideo",
        "wmv" => "video/x-ms-wmv",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    }
    .into()
}
