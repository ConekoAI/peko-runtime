//! Media files reaching the model (ADR-088): attachments on a message
//! and media a tool points at (`Read` on an image, audio, or video file).
//! A file goes to the model as a content block only when its spec takes
//! that kind and the file is within the kind's block limit; otherwise the
//! model reads a note saying it cannot take it.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use peko_message::media::{unsupported_note, MediaKind};
use peko_message::{ContentBlock, LlmMessage};
use peko_providers::spec::ModelSpec;

/// A file as a media block of `kind`, or why it cannot be one.
pub async fn load_block(path: &Path, mime: &str, kind: MediaKind) -> Result<ContentBlock, String> {
    let size = tokio::fs::metadata(path)
        .await
        .map_err(|e| format!("{}: {e}", path.display()))?
        .len();
    if size > kind.max_block_bytes() {
        return Err(format!(
            "{} is too large to send to the model ({} bytes; the limit is {})",
            path.display(),
            size,
            kind.max_block_bytes()
        ));
    }
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(kind.block(
        base64::engine::general_purpose::STANDARD.encode(bytes),
        mime.to_string(),
    ))
}

/// A media file a tool's output points at: `{"media": {"kind", "mime",
/// "path"}}`. The output itself stays text; the loop attaches the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolMedia {
    pub kind: MediaKind,
    pub mime: String,
    pub path: PathBuf,
}

impl ToolMedia {
    /// The media reference in a tool's JSON output, if any.
    #[must_use]
    pub fn from_output(output: &serde_json::Value) -> Option<Self> {
        let media = output.get("media")?;
        Some(Self {
            kind: MediaKind::parse(media.get("kind")?.as_str()?)?,
            mime: media.get("mime")?.as_str()?.to_string(),
            path: PathBuf::from(media.get("path")?.as_str()?),
        })
    }
}

/// Put `media` into the tool result `message` for a model with `spec`:
/// the file as a block inside the `ToolResult` when the model takes it,
/// else a note. Only the in-flight conversation carries the block; the
/// session keeps the tool's text.
pub async fn attach_to_tool_result(
    message: &mut LlmMessage,
    media: &ToolMedia,
    spec: Option<&ModelSpec>,
) {
    let addition = if spec.is_some_and(|spec| spec.takes(media.kind)) {
        match load_block(&media.path, &media.mime, media.kind).await {
            Ok(block) => block,
            Err(reason) => ContentBlock::Text {
                text: format!("[Not sent to the model: {reason}]"),
            },
        }
    } else {
        ContentBlock::Text {
            text: unsupported_note(&[media.kind]),
        }
    };
    for block in &mut message.content {
        if let ContentBlock::ToolResult { content, .. } = block {
            content.push(addition);
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(dir: &Path) -> PathBuf {
        let path = dir.join("a.png");
        std::fs::write(&path, b"\x89PNG\r\n\x1a\n").unwrap();
        path
    }

    fn media(path: PathBuf) -> ToolMedia {
        ToolMedia::from_output(&serde_json::json!({
            "content": "an image",
            "media": {"kind": "image", "mime": "image/png", "path": path},
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn a_model_that_takes_images_gets_the_block() {
        let dir = tempfile::tempdir().unwrap();
        let mut message = LlmMessage::tool_result("call_1", "Read", "an image", false);
        attach_to_tool_result(
            &mut message,
            &media(png(dir.path())),
            Some(&ModelSpec::frontier_chat()),
        )
        .await;
        let ContentBlock::ToolResult { content, .. } = &message.content[0] else {
            panic!("tool result");
        };
        assert!(
            matches!(&content[1], ContentBlock::Image { mime_type, .. } if mime_type == "image/png")
        );
    }

    #[tokio::test]
    async fn any_other_model_reads_a_note() {
        let dir = tempfile::tempdir().unwrap();
        let mut message = LlmMessage::tool_result("call_1", "Read", "an image", false);
        attach_to_tool_result(
            &mut message,
            &media(png(dir.path())),
            Some(&ModelSpec::text_only()),
        )
        .await;
        let ContentBlock::ToolResult { content, .. } = &message.content[0] else {
            panic!("tool result");
        };
        assert!(
            matches!(&content[1], ContentBlock::Text { text } if text.contains("cannot take images"))
        );
    }
}
