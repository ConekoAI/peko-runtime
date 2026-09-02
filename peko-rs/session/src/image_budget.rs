//! `peko_session::image_budget` — image token-budget estimator.
//!
// Closes the F21 blind spot where `ContentBlock::Image` blocks were
//! treated as ~50 chars (~21 tokens) regardless of actual size. A
//! 1024×1024 image is ~1500 real tokens; the old estimate let
//! compaction trigger late and overflow the context window.
//!
// # Three-tier priority
//!
// 1. `source.dimensions` present → `ceil(width * height / 750)`.
//    Matches OpenAI's high-detail tile count: 512px tiles + a base
//!    token for the first image. For typical resolutions this gives:
//    // 512×512 → 350, 1024×1024 → 1398, 2048×2048 → 5595.
// 2. `Base64 { data }` bytes → `ceil(data.len() / 0.75)` (roughly the
//!    number of bytes is ~0.75 of the token count after BPE encoding).
// 3. URL + mime-type table fallback (jpeg ≈ 1500, png ≈ 2500, webp ≈
//    1500, gif ≈ 1500, others → 1500). The table is conservative —
//    overestimate so compaction triggers on time rather than late.
//!
// # Migration note
//!
// The tier-1 formula matches `peko_message::image_token_estimate` (the
//! engine-side estimator already wrapped by `ImageDimensions::high_detail_tokens`).
//! We re-export that for the dimensions path so the source-of-truth
//! stays in `peko-message`.

use peko_message::{ImageSource, MessageRole};

/// Conservative token estimate for an image, used by the F21
/// retention-budget estimator. See module docs for the priority
/// order.
pub fn estimate_image_tokens(source: &ImageSource, mime_type: &str) -> usize {
    // Tier 1: explicit dimensions (preferred). Zero-dimension
    // dimensions (corrupt IHDR) are treated as absent and fall
    // through to tier 2/3 — a 0×0 image would otherwise yield 0
    // tokens and silently shrink the retention budget.
    if let Some(dims) = image_dimensions(source).filter(|d| d.width > 0 && d.height > 0) {
        return dims.high_detail_tokens();
    }

    // Tier 2: base64 bytes → tokens ≈ bytes / 0.75.
    if let ImageSource::Base64 { data, .. } = source {
        return ((data.len() as f64) / 0.75).ceil() as usize;
    }

    // Tier 3: URL + mime-type fallback table.
    mime_type_token_floor(mime_type)
}

fn image_dimensions(source: &ImageSource) -> Option<peko_message::ImageDimensions> {
    match source {
        ImageSource::Base64 { dimensions, .. } | ImageSource::Url { dimensions, .. } => *dimensions,
    }
}

/// Conservative fallback table keyed by mime_type. Returns 1500 when
/// the mime_type isn't recognised — overestimating is preferred to
/// underestimating for compaction budgeting (we want to compact
/// earlier, not later).
fn mime_type_token_floor(mime_type: &str) -> usize {
    match mime_type.to_ascii_lowercase().as_str() {
        "image/jpeg" | "image/jpg" | "image/webp" | "image/gif" => 1500,
        "image/png" | "image/heic" | "image/heif" => 2500,
        // Unknown mime — conservatively pick the larger of the
        // common table values so we never under-budget.
        _ => 2500,
    }
}

/// Convenience wrapper: estimate from a `ContentBlock::Image`-style
/// (source, mime_type) pair as it appears in `LlmMessage` content.
pub fn estimate_image_block_tokens(source: &ImageSource, mime_type: &str) -> usize {
    estimate_image_tokens(source, mime_type)
}

/// Hook for callers that want to sum image costs across an entire
/// `LlmMessage`. Currently unused outside tests but documents the intended
/// use site (compaction history summarization).
pub fn message_image_tokens(
    role: MessageRole,
    content: &[peko_message::ContentBlock],
) -> usize {
    let _ = role; // reserved — system/user may be prioritised in PR 2/3
    content
        .iter()
        .filter_map(|block| match block {
            peko_message::ContentBlock::Image { source, mime_type } => {
                Some(estimate_image_tokens(source, mime_type))
            }
            _ => None,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use peko_message::{ContentBlock, ImageDimensions, ImageSource, LlmMessage, MessageRole};

    fn base64_src(data: &str) -> ImageSource {
        ImageSource::Base64 {
            data: data.to_string(),
            dimensions: None,
        }
    }

    fn url_src(url: &str) -> ImageSource {
        ImageSource::Url {
            url: url.to_string(),
            dimensions: None,
        }
    }

    #[test]
    fn tier1_with_dimensions_wins_over_bytes() {
        // 1024×1024 → 1399 tokens by tile math
        // ((1024*1024 + 749) / 750 = 1399 remainder 75). Even with
        // a 100 KB base64 payload that would otherwise be ~133k
        // tokens, the explicit dimension wins.
        let src = ImageSource::Base64 {
            data: "x".repeat(100_000),
            dimensions: Some(ImageDimensions {
                width: 1024,
                height: 1024,
            }),
        };
        assert_eq!(estimate_image_tokens(&src, "image/png"), 1399);
    }

    #[test]
    fn tier2_base64_bytes_when_no_dimensions() {
        // 750 bytes (1.0 KB ASCII) → 1000 tokens by bytes/0.75.
        let src = base64_src(&"x".repeat(750));
        assert_eq!(estimate_image_tokens(&src, "image/png"), 1000);
    }

    #[test]
    fn tier3_url_jpeg_floor() {
        let src = url_src("https://example.com/x.jpg");
        assert_eq!(estimate_image_tokens(&src, "image/jpeg"), 1500);
    }

    #[test]
    fn tier3_url_png_floor() {
        let src = url_src("https://example.com/x.png");
        assert_eq!(estimate_image_tokens(&src, "image/png"), 2500);
    }

    #[test]
    fn tier3_url_unknown_mime_floor() {
        // Unknown → 2500 (the conservative upper of common table values).
        let src = url_src("https://example.com/x.tiff");
        assert_eq!(estimate_image_tokens(&src, "image/tiff"), 2500);
    }

    #[test]
    fn url_without_dimensions_skips_byte_path() {
        // URLs have no payload, so tier 2 (Base64 only) shouldn't fire.
        // The dimensions are absent → must reach tier 3.
        let src = url_src("https://example.com/x.png");
        let estimate = estimate_image_tokens(&src, "image/png");
        assert_eq!(estimate, 2500); // tier 3 table, not 0
    }

    #[test]
    fn message_image_tokens_sums_content_blocks() {
        let msg = LlmMessage {
            role: MessageRole::User,
            content: vec![
                ContentBlock::Image {
                    source: base64_src(&"x".repeat(750)),
                    mime_type: "image/png".to_string(),
                },
                ContentBlock::Image {
                    source: url_src("https://x.jpg"),
                    mime_type: "image/jpeg".to_string(),
                },
                ContentBlock::Text {
                    text: "what is this?".to_string(),
                },
            ],
            ..Default::default()
        };
        // 1000 (base64) + 1500 (jpeg floor) = 2500. Text contributes 0.
        assert_eq!(
            message_image_tokens(MessageRole::User, &msg.content),
            2500
        );
    }

    #[test]
    fn explicit_zero_dimensions_falls_through() {
        // 0×0 dimensions are corrupt — they're filtered out so tier 1
        // doesn't return 0 (which would silently shrink the retention
        // budget). Falls through to tier 2 (base64 bytes/0.75).
        let src = ImageSource::Base64 {
            data: "x".repeat(750),
            dimensions: Some(ImageDimensions { width: 0, height: 0 }),
        };
        assert_eq!(estimate_image_tokens(&src, "image/png"), 1000);
    }
}