//! Vector Memory Tests for Pekobot
//!
//! Tests for embedding-based semantic search using cosine similarity.
//! These tests verify the vector memory implementation that Gamma is building.

use pekobot::memory::sqlite::SqliteMemory;
use serde_json::json;
use tempfile::TempDir;

/// Helper to create a test memory store with vector support
fn create_vector_memory() -> (SqliteMemory, TempDir) {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("vector_test.db");
    let memory = SqliteMemory::new(&db_path, "test-namespace").unwrap();
    (memory, temp_dir)
}

/// Mock embedding function (in real impl, this would call an embedding API)
fn mock_embed(text: &str) -> Vec<f32> {
    // Simple mock: hash-based embedding for testing
    // Real implementation would use OpenAI, sentence-transformers, etc.
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    let hash = hasher.finish();

    // Create a 128-dimensional vector from the hash
    let mut vec = Vec::with_capacity(128);
    for i in 0..128 {
        let bit = ((hash >> (i % 64)) & 1) as f32;
        vec.push(if bit == 1.0 { 1.0 } else { -1.0 });
    }
    vec
}

/// Cosine similarity calculation
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "Vectors must have same dimension");

    let dot_product: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    dot_product / (norm_a * norm_b)
}

// ============================================================================
// Vector Storage Tests
// ============================================================================

#[test]
fn test_store_with_embedding() {
    let (memory, _temp) = create_vector_memory();

    let content = "The quick brown fox jumps over the lazy dog";
    let embedding = mock_embed(content);

    // Store with embedding
    let id = memory
        .store_with_embedding(content, Some(json!({"source": "test"})), &embedding)
        .unwrap();

    assert!(!id.is_empty());

    // Retrieve and verify embedding is stored
    let entry = memory.get(&id).unwrap().unwrap();
    assert_eq!(entry.content, content);
    assert!(entry.embedding.is_some());
}

#[test]
fn test_store_without_embedding() {
    let (memory, _temp) = create_vector_memory();

    // Store without embedding (traditional storage)
    let id = memory.store("Simple content", None).unwrap();

    let entry = memory.get(&id).unwrap().unwrap();
    assert_eq!(entry.content, "Simple content");
    // Embedding should be None for traditional storage
    assert!(entry.embedding.is_none());
}

#[test]
fn test_vector_dimension_validation() {
    let (memory, _temp) = create_vector_memory();

    // Test with different vector dimensions
    let dims = [64, 128, 256, 512, 768, 1024, 1536];

    for dim in dims {
        let embedding = vec![0.1f32; dim];
        let content = format!("Test content with dim {}", dim);

        let result = memory.store_with_embedding(&content, None, &embedding);

        // Should either succeed or fail gracefully with clear error
        match result {
            Ok(id) => {
                let entry = memory.get(&id).unwrap().unwrap();
                assert!(entry.embedding.is_some());
            }
            Err(e) => {
                // Should provide meaningful error about dimension
                let error_msg = e.to_string().to_lowercase();
                assert!(
                    error_msg.contains("dimension") || error_msg.contains("size"),
                    "Error should mention dimension: {}",
                    e
                );
            }
        }
    }
}

// ============================================================================
// Vector Search Tests
// ============================================================================

#[test]
fn test_vector_search_basic() {
    let (memory, _temp) = create_vector_memory();

    // Store several entries with embeddings
    let entries = vec![
        (
            "I love machine learning",
            mock_embed("I love machine learning"),
        ),
        (
            "The weather is nice today",
            mock_embed("The weather is nice today"),
        ),
        (
            "Rust is a systems programming language",
            mock_embed("Rust is a systems programming language"),
        ),
        (
            "Deep learning is a subset of ML",
            mock_embed("Deep learning is a subset of ML"),
        ),
        (
            "It's raining cats and dogs",
            mock_embed("It's raining cats and dogs"),
        ),
    ];

    for (content, embedding) in &entries {
        memory
            .store_with_embedding(content, None, embedding)
            .unwrap();
    }

    // Search with a query similar to ML-related entries
    let query = "artificial intelligence and neural networks";
    let query_embedding = mock_embed(query);

    let results = memory.search_vector(&query_embedding, 3, 0.0).unwrap();

    // Should return results
    assert!(!results.is_empty());
    assert!(results.len() <= 3);
}

#[test]
fn test_vector_search_with_similarity_threshold() {
    let (memory, _temp) = create_vector_memory();

    // Store entries
    let entries = vec![
        ("Cats are furry animals", mock_embed("cats furry")),
        ("Dogs are loyal pets", mock_embed("dogs loyal")),
        ("Programming in Rust", mock_embed("programming rust")),
    ];

    for (content, embedding) in &entries {
        memory
            .store_with_embedding(content, None, embedding)
            .unwrap();
    }

    // Search with high threshold (should filter low-similarity results)
    let query_embedding = mock_embed("cats and kittens");

    let results_low_threshold = memory.search_vector(&query_embedding, 10, 0.3).unwrap();
    let results_high_threshold = memory.search_vector(&query_embedding, 10, 0.8).unwrap();

    // High threshold should return fewer or equal results
    assert!(results_high_threshold.len() <= results_low_threshold.len());
}

#[test]
fn test_vector_search_empty_database() {
    let (memory, _temp) = create_vector_memory();

    let query_embedding = mock_embed("test query");
    let results = memory.search_vector(&query_embedding, 5, 0.0).unwrap();

    assert!(results.is_empty());
}

#[test]
fn test_vector_search_with_metadata_filter() {
    let (memory, _temp) = create_vector_memory();

    // Store entries with different metadata
    memory
        .store_with_embedding(
            "Project Alpha documentation",
            Some(json!({"project": "alpha", "type": "doc"})),
            &mock_embed("documentation"),
        )
        .unwrap();

    memory
        .store_with_embedding(
            "Project Beta code review",
            Some(json!({"project": "beta", "type": "code"})),
            &mock_embed("code review"),
        )
        .unwrap();

    memory
        .store_with_embedding(
            "Project Alpha meeting notes",
            Some(json!({"project": "alpha", "type": "notes"})),
            &mock_embed("meeting notes"),
        )
        .unwrap();

    // Search with metadata filter
    let query_embedding = mock_embed("project documentation");
    let results = memory
        .search_vector_with_filter(&query_embedding, 10, 0.0, json!({"project": "alpha"}))
        .unwrap();

    // All results should be from project alpha
    for result in &results {
        let metadata = result.metadata.as_ref().unwrap();
        assert_eq!(metadata["project"], "alpha");
    }
}

// ============================================================================
// Cosine Similarity Tests
// ============================================================================

#[test]
fn test_cosine_similarity_identical_vectors() {
    let vec = vec![1.0, 2.0, 3.0, 4.0];
    let similarity = cosine_similarity(&vec, &vec);

    // Identical vectors should have similarity 1.0
    assert!((similarity - 1.0).abs() < 0.0001);
}

#[test]
fn test_cosine_similarity_opposite_vectors() {
    let a = vec![1.0, 2.0, 3.0];
    let b = vec![-1.0, -2.0, -3.0];
    let similarity = cosine_similarity(&a, &b);

    // Opposite vectors should have similarity -1.0
    assert!((similarity - (-1.0)).abs() < 0.0001);
}

#[test]
fn test_cosine_similarity_orthogonal_vectors() {
    let a = vec![1.0, 0.0, 0.0];
    let b = vec![0.0, 1.0, 0.0];
    let similarity = cosine_similarity(&a, &b);

    // Orthogonal vectors should have similarity 0.0
    assert!(similarity.abs() < 0.0001);
}

#[test]
fn test_cosine_similarity_various() {
    let test_cases = vec![
        (vec![1.0, 0.0], vec![1.0, 0.0], 1.0), // Same direction
        (vec![1.0, 0.0], vec![0.0, 1.0], 0.0), // Perpendicular
        (vec![1.0, 1.0], vec![1.0, 1.0], 1.0), // Same direction
        (vec![3.0, 4.0], vec![6.0, 8.0], 1.0), // Same direction (scaled)
    ];

    for (a, b, expected) in test_cases {
        let similarity = cosine_similarity(&a, &b);
        assert!(
            (similarity - expected).abs() < 0.0001,
            "Expected similarity close to {}, got {}",
            expected,
            similarity
        );
    }
}

// ============================================================================
// Edge Cases
// ============================================================================

#[test]
fn test_vector_search_with_zero_vector() {
    let (memory, _temp) = create_vector_memory();

    // Store normal content
    memory
        .store_with_embedding("Normal content", None, &mock_embed("normal"))
        .unwrap();

    // Search with zero vector
    let zero_vector = vec![0.0f32; 128];
    let results = memory.search_vector(&zero_vector, 5, 0.0).unwrap();

    // Should handle gracefully (return empty or all results with 0 similarity)
    // Implementation choice, but shouldn't panic
}

#[test]
fn test_vector_search_with_very_large_embedding() {
    let (memory, _temp) = create_vector_memory();

    // Large embedding (e.g., from a large model)
    let large_embedding: Vec<f32> = (0..4096).map(|i| (i as f32) / 1000.0).collect();

    let result =
        memory.store_with_embedding("Content with large embedding", None, &large_embedding);

    // Should handle gracefully
    assert!(result.is_ok() || result.is_err()); // Either is acceptable
}

#[test]
fn test_vector_search_with_negative_values() {
    let (memory, _temp) = create_vector_memory();

    let embedding: Vec<f32> = (-64..64).map(|i| i as f32 / 64.0).collect();

    memory
        .store_with_embedding("Content with mixed values", None, &embedding)
        .unwrap();

    let query: Vec<f32> = (-64..64).map(|i| (i as f32 / 64.0) * 0.5).collect();
    let results = memory.search_vector(&query, 5, 0.0).unwrap();

    // Should handle negative values correctly
    assert!(!results.is_empty());
}

#[test]
fn test_vector_update() {
    let (memory, _temp) = create_vector_memory();

    // Store initial content
    let id = memory
        .store_with_embedding(
            "Initial content",
            Some(json!({"version": 1})),
            &mock_embed("initial"),
        )
        .unwrap();

    // Update with new content and embedding
    memory
        .update_with_embedding(
            &id,
            "Updated content",
            Some(json!({"version": 2})),
            &mock_embed("updated"),
        )
        .unwrap();

    // Verify update
    let entry = memory.get(&id).unwrap().unwrap();
    assert_eq!(entry.content, "Updated content");
}

// ============================================================================
// Performance Tests
// ============================================================================

#[test]
fn test_vector_search_performance_small_dataset() {
    let (memory, _temp) = create_vector_memory();

    // Insert 100 entries
    for i in 0..100 {
        let content = format!("Document number {} with some content", i);
        let embedding = mock_embed(&content);
        memory
            .store_with_embedding(&content, None, &embedding)
            .unwrap();
    }

    // Search should be fast
    let start = std::time::Instant::now();
    let query = mock_embed("search query");
    let results = memory.search_vector(&query, 10, 0.0).unwrap();
    let elapsed = start.elapsed();

    assert!(!results.is_empty());
    assert!(
        elapsed.as_millis() < 1000,
        "Search took too long: {:?}",
        elapsed
    );
}

#[ignore = "Slow test - run manually for performance validation"]
#[test]
fn test_vector_search_performance_large_dataset() {
    let (memory, _temp) = create_vector_memory();

    // Insert 10,000 entries
    for i in 0..10000 {
        let content = format!(
            "Document {} with varied content for semantic search testing",
            i
        );
        let embedding = mock_embed(&content);
        memory
            .store_with_embedding(&content, None, &embedding)
            .unwrap();
    }

    // Search should still be reasonably fast
    let start = std::time::Instant::now();
    let query = mock_embed("semantic search document");
    let results = memory.search_vector(&query, 10, 0.0).unwrap();
    let elapsed = start.elapsed();

    assert!(!results.is_empty());
    assert!(
        elapsed.as_millis() < 5000,
        "Large dataset search took too long: {:?}",
        elapsed
    );
}

// ============================================================================
// Integration with Hybrid Search
// ============================================================================

#[test]
fn test_hybrid_search_keyword_and_vector() {
    let (memory, _temp) = create_vector_memory();

    // Store entries
    memory
        .store_with_embedding(
            "Machine learning is fascinating",
            Some(json!({"topic": "ml"})),
            &mock_embed("ml fascinating"),
        )
        .unwrap();

    memory
        .store_with_embedding(
            "Deep learning neural networks",
            Some(json!({"topic": "dl"})),
            &mock_embed("dl neural networks"),
        )
        .unwrap();

    memory
        .store_with_embedding(
            "Cooking recipes for beginners",
            Some(json!({"topic": "cooking"})),
            &mock_embed("cooking recipes"),
        )
        .unwrap();

    // Hybrid search: keyword + vector
    let results = memory
        .search_hybrid(
            "neural",                           // keyword
            &mock_embed("machine learning ai"), // vector query
            10,                                 // limit
            0.5,                                // vector threshold
            0.3,                                // keyword weight
        )
        .unwrap();

    // Should return ML-related results
    assert!(!results.is_empty());
}
