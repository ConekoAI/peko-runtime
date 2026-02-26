//! Hybrid search - combines BM25 keyword relevance with vector similarity
//!
//! BM25 is strong at exact token matches (IDs, code symbols, error strings)
//! Vector search is strong at semantic similarity (paraphrases, meaning)
//! Hybrid combines both for better retrieval.

use crate::memory::vector::VectorMemory;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::debug;

/// Hybrid search configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HybridSearchConfig {
    /// Enable hybrid search
    pub enabled: bool,
    /// Weight for vector similarity (0.0 - 1.0)
    pub vector_weight: f32,
    /// Weight for text/BM25 score (0.0 - 1.0)
    pub text_weight: f32,
    /// Multiplier for candidate pool size
    pub candidate_multiplier: usize,
    /// Maximum results to return
    pub max_results: usize,
}

impl Default for HybridSearchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            vector_weight: 0.7,
            text_weight: 0.3,
            candidate_multiplier: 4,
            max_results: 10,
        }
    }
}

impl HybridSearchConfig {
    /// Create with normalized weights (ensure they sum to 1.0)
    #[must_use]
    pub fn with_weights(vector_weight: f32, text_weight: f32) -> Self {
        let total = vector_weight + text_weight;
        let normalized_vector = if total > 0.0 {
            vector_weight / total
        } else {
            0.7
        };
        let normalized_text = if total > 0.0 {
            text_weight / total
        } else {
            0.3
        };

        Self {
            vector_weight: normalized_vector,
            text_weight: normalized_text,
            ..Default::default()
        }
    }
}

/// A candidate result with both scores
#[derive(Debug, Clone)]
pub struct HybridCandidate {
    /// Entry ID
    pub id: String,
    /// Entry content
    pub content: String,
    /// Vector similarity score (0.0 - 1.0)
    pub vector_score: f32,
    /// BM25 text score (normalized 0.0 - 1.0)
    pub text_score: f32,
    /// Final combined score
    pub combined_score: f32,
    /// Embedding model used
    pub embedding_model: Option<String>,
}

/// BM25 scorer for keyword relevance
pub struct BM25Scorer {
    /// Average document length
    avgdl: f32,
    /// Document frequencies per term
    doc_freqs: std::collections::HashMap<String, usize>,
    /// Total documents
    total_docs: usize,
    /// k1 parameter (term saturation)
    k1: f32,
    /// b parameter (length normalization)
    b: f32,
}

impl BM25Scorer {
    /// Create a new BM25 scorer
    #[must_use]
    pub fn new() -> Self {
        Self {
            avgdl: 100.0, // Default average document length
            doc_freqs: std::collections::HashMap::new(),
            total_docs: 0,
            k1: 1.2,
            b: 0.75,
        }
    }

    /// Calculate BM25 score for a document
    #[must_use]
    pub fn score(&self, query_terms: &[String], doc_content: &str, doc_length: usize) -> f32 {
        let mut score = 0.0;

        for term in query_terms {
            let tf = term_frequency(term, doc_content);
            let idf = self.idf(term);

            let numerator = tf * (self.k1 + 1.0);
            let denominator =
                tf + self.k1 * (1.0 - self.b + self.b * (doc_length as f32 / self.avgdl));

            score += idf * (numerator / denominator);
        }

        score
    }

    /// Inverse document frequency
    fn idf(&self, term: &str) -> f32 {
        let df = self.doc_freqs.get(term).copied().unwrap_or(1) as f32;
        let n = self.total_docs as f32;

        // IDF = log((N - df + 0.5) / (df + 0.5) + 1)
        ((n - df + 0.5) / (df + 0.5) + 1.0).ln()
    }

    /// Normalize BM25 scores to 0-1 range
    #[must_use]
    pub fn normalize_score(score: f32, max_score: f32) -> f32 {
        if max_score <= 0.0 {
            return 0.0;
        }
        // Use sigmoid-like normalization
        let normalized = score / (1.0 + score.abs());
        normalized.clamp(0.0, 1.0)
    }
}

impl Default for BM25Scorer {
    fn default() -> Self {
        Self::new()
    }
}

/// Count term frequency in content
fn term_frequency(term: &str, content: &str) -> f32 {
    let content_lower = content.to_lowercase();
    let term_lower = term.to_lowercase();

    let count = content_lower.matches(&term_lower).count();
    count as f32
}

/// Tokenize query into terms
fn tokenize_query(query: &str) -> Vec<String> {
    query
        .to_lowercase()
        .split_whitespace()
        .map(std::string::ToString::to_string)
        .filter(|s| s.len() > 2) // Filter out very short terms
        .collect()
}

/// Hybrid searcher that combines vector and text search
pub struct HybridSearcher {
    config: HybridSearchConfig,
    bm25: BM25Scorer,
}

impl HybridSearcher {
    /// Create a new hybrid searcher
    #[must_use]
    pub fn new(config: HybridSearchConfig) -> Self {
        Self {
            config,
            bm25: BM25Scorer::new(),
        }
    }

    /// Create with default config
    #[must_use]
    pub fn default_config() -> Self {
        Self::new(HybridSearchConfig::default())
    }

    /// Perform hybrid search
    ///
    /// Algorithm:
    /// 1. Get candidate pool from vector search
    /// 2. Calculate BM25 scores for candidates
    /// 3. Combine scores with weights
    /// 4. Rank by combined score
    pub async fn search(
        &self,
        vector_memory: &VectorMemory,
        query: &str,
        query_embedding: &[f32],
    ) -> Result<Vec<HybridCandidate>> {
        let query_terms = tokenize_query(query);

        if query_terms.is_empty() {
            // Fall back to pure vector search
            debug!("No query terms, using pure vector search");
            let results =
                vector_memory.search_similar(query_embedding, self.config.max_results, -1.0)?;

            return Ok(results
                .into_iter()
                .map(|r| HybridCandidate {
                    id: r.entry.id,
                    content: r.entry.content,
                    vector_score: r.similarity,
                    text_score: 0.0,
                    combined_score: r.similarity * self.config.vector_weight,
                    embedding_model: r.embedding_model,
                })
                .collect());
        }

        // Step 1: Get expanded candidate pool from vector search
        let candidate_limit = self.config.max_results * self.config.candidate_multiplier;
        let vector_results = vector_memory.search_similar(
            query_embedding,
            candidate_limit,
            -1.0, // Include all similarities
        )?;

        debug!("Vector search returned {} candidates", vector_results.len());

        // Step 2: Calculate BM25 scores and combine
        let mut candidates: Vec<HybridCandidate> = vector_results
            .into_iter()
            .map(|result| {
                let text_score = if self.config.text_weight > 0.0 {
                    let bm25_raw = self.bm25.score(
                        &query_terms,
                        &result.entry.content,
                        result.entry.content.len(),
                    );
                    BM25Scorer::normalize_score(bm25_raw, 10.0)
                } else {
                    0.0
                };

                // Combined score = weighted sum
                let combined = result.similarity * self.config.vector_weight
                    + text_score * self.config.text_weight;

                HybridCandidate {
                    id: result.entry.id,
                    content: result.entry.content,
                    vector_score: result.similarity,
                    text_score,
                    combined_score: combined,
                    embedding_model: result.embedding_model,
                }
            })
            .collect();

        // Step 3: Sort by combined score (descending)
        candidates.sort_by(|a, b| {
            b.combined_score
                .partial_cmp(&a.combined_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Step 4: Return top results
        candidates.truncate(self.config.max_results);

        debug!(
            "Hybrid search returning {} results (vector_weight: {:.2}, text_weight: {:.2})",
            candidates.len(),
            self.config.vector_weight,
            self.config.text_weight
        );

        Ok(candidates)
    }

    /// Update configuration
    pub fn update_config(&mut self, config: HybridSearchConfig) {
        self.config = config;
    }
}

impl Default for HybridSearcher {
    fn default() -> Self {
        Self::default_config()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hybrid_config_weights() {
        let config = HybridSearchConfig::with_weights(0.8, 0.2);

        // Should be normalized to sum to 1.0
        let sum = config.vector_weight + config.text_weight;
        assert!((sum - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_tokenize_query() {
        let terms = tokenize_query("Hello world test query");
        assert_eq!(terms, vec!["hello", "world", "test", "query"]);
    }

    #[test]
    fn test_tokenize_query_filters_short() {
        let terms = tokenize_query("a big cat");
        assert!(!terms.contains(&"a".to_string())); // Too short
        assert!(terms.contains(&"big".to_string()));
        assert!(terms.contains(&"cat".to_string()));
    }

    #[test]
    fn test_term_frequency() {
        let content = "the quick brown fox jumps over the lazy dog";

        assert_eq!(term_frequency("the", content), 2.0);
        assert_eq!(term_frequency("fox", content), 1.0);
        assert_eq!(term_frequency("missing", content), 0.0);
    }

    #[test]
    fn test_bm25_normalize() {
        assert_eq!(BM25Scorer::normalize_score(0.0, 10.0), 0.0);

        let normalized = BM25Scorer::normalize_score(5.0, 10.0);
        assert!(normalized > 0.0 && normalized <= 1.0);
    }

    #[test]
    fn test_hybrid_searcher_creation() {
        let searcher = HybridSearcher::default_config();
        assert!(searcher.config.enabled);
        assert_eq!(searcher.config.max_results, 10);
    }
}
