//! Block chunker for streaming responses
//!
//! Implements OpenClaw-style block streaming:
//! - Emits coarse-grained blocks (not token-by-token)
//! - Respects min/max character bounds
//! - Breaks at natural boundaries (paragraph/sentence/whitespace)

use tracing::debug;

/// Break preference for block boundaries
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakPreference {
    /// Break at paragraph boundaries (double newline)
    Paragraph,
    /// Break at sentence boundaries (period + space)
    Sentence,
    /// Break at whitespace
    Whitespace,
    /// Hard break at max_chars
    Hard,
}

impl Default for BreakPreference {
    fn default() -> Self {
        BreakPreference::Sentence
    }
}

/// Block chunker configuration
#[derive(Debug, Clone)]
pub struct ChunkerConfig {
    /// Minimum characters before emitting a block
    pub min_chars: usize,
    /// Maximum characters per block
    pub max_chars: usize,
    /// Break preference for finding boundaries
    pub break_preference: BreakPreference,
    /// Whether to emit partial blocks when buffer is flushed
    pub emit_partial: bool,
}

impl Default for ChunkerConfig {
    fn default() -> Self {
        Self {
            min_chars: 100,
            max_chars: 2000,
            break_preference: BreakPreference::Sentence,
            emit_partial: true,
        }
    }
}

/// Block chunker for streaming text
///
/// Accumulates text and emits blocks when bounds are met.
pub struct BlockChunker {
    config: ChunkerConfig,
    buffer: String,
    emitted_chars: usize,
}

impl BlockChunker {
    /// Create a new block chunker with default config
    pub fn new() -> Self {
        Self::with_config(ChunkerConfig::default())
    }

    /// Create a new block chunker with custom config
    pub fn with_config(config: ChunkerConfig) -> Self {
        Self {
            config,
            buffer: String::new(),
            emitted_chars: 0,
        }
    }

    /// Feed text into the chunker
    ///
    /// Returns any complete blocks that should be emitted.
    pub fn feed(&mut self, text: &str) -> Vec<String> {
        self.buffer.push_str(text);
        self.extract_blocks()
    }

    /// Flush remaining buffer as final block(s)
    ///
    /// This should be called when the stream ends.
    pub fn flush(&mut self) -> Vec<String> {
        let mut blocks = Vec::new();

        // Emit remaining buffer if not empty
        if !self.buffer.is_empty() {
            if self.config.emit_partial || self.buffer.len() >= self.config.min_chars {
                blocks.push(self.buffer.clone());
            }
            self.buffer.clear();
        }

        blocks
    }

    /// Get the current buffer size
    pub fn buffer_len(&self) -> usize {
        self.buffer.len()
    }

    /// Extract complete blocks from the buffer
    fn extract_blocks(&mut self) -> Vec<String> {
        let mut blocks = Vec::new();

        while self.buffer.len() >= self.config.min_chars {
            // Find the best break point
            let break_point = self.find_break_point();

            if let Some(pos) = break_point {
                if pos >= self.config.min_chars {
                    // Extract block
                    let block = self.buffer[..pos].to_string();
                    self.buffer = self.buffer[pos..].to_string();
                    self.emitted_chars += block.len();
                    blocks.push(block);
                } else {
                    // Can't find a good break point, need more data
                    break;
                }
            } else {
                // No break point found within bounds
                if self.buffer.len() >= self.config.max_chars {
                    // Force break at max_chars
                    let block = self.buffer[..self.config.max_chars].to_string();
                    self.buffer = self.buffer[self.config.max_chars..].to_string();
                    self.emitted_chars += block.len();
                    blocks.push(block);
                } else {
                    // Wait for more data
                    break;
                }
            }
        }

        blocks
    }

    /// Find the best break point in the buffer
    fn find_break_point(&self) -> Option<usize> {
        let search_limit = self.buffer.len().min(self.config.max_chars);

        match self.config.break_preference {
            BreakPreference::Paragraph => self.find_paragraph_break(search_limit),
            BreakPreference::Sentence => {
                self.find_sentence_break(search_limit)
                    .or_else(|| self.find_whitespace_break(search_limit))
            }
            BreakPreference::Whitespace => self.find_whitespace_break(search_limit),
            BreakPreference::Hard => Some(search_limit),
        }
    }

    /// Find paragraph break (double newline)
    fn find_paragraph_break(&self, limit: usize) -> Option<usize> {
        let search_area = &self.buffer[..limit];

        // Look for "\n\n" or "\r\n\r\n"
        search_area.find("\n\n").map(|pos| pos + 2)
            .or_else(|| search_area.find("\r\n\r\n").map(|pos| pos + 4))
    }

    /// Find sentence break (period + space + uppercase)
    fn find_sentence_break(&self, limit: usize) -> Option<usize> {
        let search_area = &self.buffer[..limit];

        // Look for ". " followed by uppercase letter
        for (pos, _) in search_area.match_indices(". ") {
            if pos + 2 < search_area.len() {
                let next_char = &search_area[pos + 2..pos + 3];
                if next_char.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                    return Some(pos + 2);
                }
            }
        }

        // Also check for ".\n"
        search_area.find(".\n").map(|pos| pos + 2)
    }

    /// Find whitespace break (space or newline)
    fn find_whitespace_break(&self, limit: usize) -> Option<usize> {
        let search_area = &self.buffer[..limit];

        // Find last whitespace before limit
        search_area
            .rfind(' ')
            .or_else(|| search_area.rfind('\n'))
            .map(|pos| pos + 1) // Include the whitespace in the block
    }

    /// Get total emitted character count
    pub fn emitted_chars(&self) -> usize {
        self.emitted_chars
    }

    /// Reset the chunker state
    pub fn reset(&mut self) {
        self.buffer.clear();
        self.emitted_chars = 0;
    }
}

impl Default for BlockChunker {
    fn default() -> Self {
        Self::new()
    }
}

/// Coalescing chunker that merges small blocks
///
/// Waits for idle periods or size thresholds before emitting.
pub struct CoalescingChunker {
    inner: BlockChunker,
    coalesce_buffer: String,
    min_coalesce_chars: usize,
    max_coalesce_chars: usize,
}

impl CoalescingChunker {
    /// Create a new coalescing chunker
    pub fn new() -> Self {
        Self::with_config(ChunkerConfig::default(), 1500, 3000)
    }

    /// Create with custom config
    pub fn with_config(
        chunker_config: ChunkerConfig,
        min_coalesce: usize,
        max_coalesce: usize,
    ) -> Self {
        Self {
            inner: BlockChunker::with_config(chunker_config),
            coalesce_buffer: String::new(),
            min_coalesce_chars: min_coalesce,
            max_coalesce_chars: max_coalesce,
        }
    }

    /// Feed text and get any ready blocks
    pub fn feed(&mut self, text: &str) -> Vec<String> {
        let blocks = self.inner.feed(text);

        let mut ready_blocks = Vec::new();

        for block in blocks {
            self.coalesce_buffer.push_str(&block);

            // Check if we should emit
            if self.coalesce_buffer.len() >= self.min_coalesce_chars {
                ready_blocks.push(self.coalesce_buffer.clone());
                self.coalesce_buffer.clear();
            }
        }

        ready_blocks
    }

    /// Flush remaining content
    pub fn flush(&mut self) -> Vec<String> {
        let blocks = self.inner.flush();

        for block in blocks {
            self.coalesce_buffer.push_str(&block);
        }

        let mut result = Vec::new();
        if !self.coalesce_buffer.is_empty() {
            result.push(self.coalesce_buffer.clone());
            self.coalesce_buffer.clear();
        }

        result
    }

    /// Check if there's pending content to flush after idle
    pub fn check_idle_flush(&mut self) -> Option<String> {
        if !self.coalesce_buffer.is_empty() {
            let result = self.coalesce_buffer.clone();
            self.coalesce_buffer.clear();
            Some(result)
        } else {
            None
        }
    }
}

impl Default for CoalescingChunker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_chunking() {
        let mut chunker = BlockChunker::with_config(ChunkerConfig {
            min_chars: 10,
            max_chars: 50,
            break_preference: BreakPreference::Whitespace,
            emit_partial: true,
        });

        // Feed text that exceeds min_chars
        let blocks = chunker.feed("Hello world this is a test of the block chunker system. ");

        assert!(!blocks.is_empty());
        assert!(blocks[0].len() >= 10);
    }

    #[test]
    fn test_sentence_break() {
        let mut chunker = BlockChunker::with_config(ChunkerConfig {
            min_chars: 20,
            max_chars: 100,
            break_preference: BreakPreference::Sentence,
            emit_partial: true,
        });

        let blocks = chunker.feed(
            "First sentence here. Second sentence here. Third sentence here."
        );

        // Should break at sentence boundaries
        assert!(!blocks.is_empty());
        // First block should end with "here. "
        assert!(blocks[0].contains("First sentence"));
    }

    #[test]
    fn test_flush_partial() {
        let mut chunker = BlockChunker::with_config(ChunkerConfig {
            min_chars: 100,
            max_chars: 200,
            break_preference: BreakPreference::Whitespace,
            emit_partial: true,
        });

        // Feed text below min_chars
        chunker.feed("Short text.");

        // Flush should emit partial
        let blocks = chunker.flush();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0], "Short text.");
    }

    #[test]
    fn test_max_chars_force_break() {
        let mut chunker = BlockChunker::with_config(ChunkerConfig {
            min_chars: 10,
            max_chars: 20,
            break_preference: BreakPreference::Paragraph,
            emit_partial: true,
        });

        // Feed long text without paragraph breaks
        let blocks = chunker.feed("This is a very long text without any paragraph breaks at all.");

        // Should force break at max_chars
        assert!(!blocks.is_empty());
        assert!(blocks[0].len() <= 20);
    }

    #[test]
    fn test_coalescing() {
        let mut chunker = CoalescingChunker::with_config(
            ChunkerConfig {
                min_chars: 10,
                max_chars: 50,
                break_preference: BreakPreference::Whitespace,
                emit_partial: true,
            },
            30, // min_coalesce
            100, // max_coalesce
        );

        // Feed small chunks
        let _ = chunker.feed("Hello world ");
        let blocks = chunker.feed("this is more text. ");

        // Should not emit until coalesce threshold
        assert!(blocks.is_empty() || blocks[0].len() < 30);

        // Flush should emit accumulated text
        let final_blocks = chunker.flush();
        assert!(!final_blocks.is_empty());
    }
}