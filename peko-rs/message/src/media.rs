//! Media a model can take as content blocks (ADR-088): images, audio,
//! and video. One place for the formats, the per-kind block limits, and
//! the note a turn gets when its model cannot take a kind — shared by
//! message attachments and tool results (`Read` on a media file).

use crate::{ContentBlock, ImageSource};

/// What kind of media a file is, for a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaKind {
    Image,
    Audio,
    Video,
}

impl MediaKind {
    /// The kind for a MIME type a model can take: the image formats
    /// every vision API reads, and the audio and video formats omni
    /// models document (MP3/WAV/FLAC/M4A/OGG, MP4/MOV/AVI/WMV/WebM).
    #[must_use]
    pub fn from_mime(mime: &str) -> Option<Self> {
        match mime {
            "image/png" | "image/jpeg" | "image/gif" | "image/webp" => Some(Self::Image),
            "audio/wav" | "audio/mpeg" | "audio/flac" | "audio/mp4" | "audio/ogg" => {
                Some(Self::Audio)
            }
            "video/mp4" | "video/quicktime" | "video/x-msvideo" | "video/x-ms-wmv"
            | "video/webm" => Some(Self::Video),
            _ => None,
        }
    }

    /// `image` / `audio` / `video`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Audio => "audio",
            Self::Video => "video",
        }
    }

    /// The inverse of [`Self::as_str`].
    #[must_use]
    pub fn parse(kind: &str) -> Option<Self> {
        match kind {
            "image" => Some(Self::Image),
            "audio" => Some(Self::Audio),
            "video" => Some(Self::Video),
            _ => None,
        }
    }

    /// How a sentence names this kind (`images`, `audio`, `video`).
    #[must_use]
    pub fn noun(self) -> &'static str {
        match self {
            Self::Image => "images",
            Self::Audio => "audio",
            Self::Video => "video",
        }
    }

    /// The largest file sent to a model as a block of this kind.
    #[must_use]
    pub fn max_block_bytes(self) -> u64 {
        match self {
            Self::Image => 5 * 1024 * 1024,
            Self::Audio | Self::Video => 20 * 1024 * 1024,
        }
    }

    /// A content block of this kind holding base64 `data`.
    #[must_use]
    pub fn block(self, data: String, mime_type: String) -> ContentBlock {
        let source = ImageSource::Base64 {
            data,
            dimensions: None,
        };
        match self {
            Self::Image => ContentBlock::Image { source, mime_type },
            Self::Audio => ContentBlock::Audio { source, mime_type },
            Self::Video => ContentBlock::Video { source, mime_type },
        }
    }
}

/// The line a turn's text gains for media its model cannot take, e.g.
/// `[The current model cannot take audio or video; …]`.
#[must_use]
pub fn unsupported_note(kinds: &[MediaKind]) -> String {
    let nouns: Vec<&str> = kinds.iter().map(|k| k.noun()).collect();
    let list = match nouns.as_slice() {
        [] => String::new(),
        [one] => (*one).to_string(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    };
    format!("[The current model cannot take {list}; those files are still there for your tools.]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_round_trip_and_name_themselves() {
        for kind in [MediaKind::Image, MediaKind::Audio, MediaKind::Video] {
            assert_eq!(MediaKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(MediaKind::from_mime("audio/mpeg"), Some(MediaKind::Audio));
        assert_eq!(MediaKind::from_mime("image/svg+xml"), None);
        assert_eq!(
            unsupported_note(&[MediaKind::Image, MediaKind::Audio, MediaKind::Video]),
            "[The current model cannot take images, audio or video; those files are still there for your tools.]"
        );
    }
}
