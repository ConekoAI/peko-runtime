//! Package validation for portable agents
//!
//! Validates .agent package integrity, checksums, and signatures.

use crate::portable::manifest::AgentManifest;

/// Validation result
#[derive(Debug, Clone)]
pub struct ValidationResult {
    /// Whether validation passed
    pub valid: bool,
    /// List of validation errors (if any)
    pub errors: Vec<ValidationError>,
    /// List of warnings (non-fatal issues)
    pub warnings: Vec<ValidationWarning>,
}

/// Validation error types
#[derive(Debug, Clone)]
pub enum ValidationError {
    /// Manifest parse error
    InvalidManifest(String),
    /// File missing from package
    MissingFile(String),
    /// Checksum mismatch
    ChecksumMismatch {
        file: String,
        expected: String,
        actual: String,
    },
    /// Invalid signature
    InvalidSignature(String),
    /// DID resolution failed
    DidResolutionFailed(String),
    /// Required file is empty
    EmptyFile(String),
    /// Package format version not supported
    UnsupportedFormatVersion { expected: String, actual: String },
}

/// Validation warning types
#[derive(Debug, Clone)]
pub enum ValidationWarning {
    /// Unknown file in package (not in manifest)
    UnknownFile(String),
    /// File listed in manifest but not critical
    OptionalFileMissing(String),
    /// Older format version (backward compatible)
    OlderFormatVersion { current: String, package: String },
    /// Memory encryption disabled
    UnencryptedMemory,
    /// Keys not encrypted
    UnencryptedKeys,
}

impl ValidationResult {
    /// Create a new successful validation result
    #[must_use] 
    pub fn success() -> Self {
        Self {
            valid: true,
            errors: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// Create a new failed validation result
    #[must_use] 
    pub fn failure(error: ValidationError) -> Self {
        Self {
            valid: false,
            errors: vec![error],
            warnings: Vec::new(),
        }
    }

    /// Add an error
    pub fn add_error(&mut self, error: ValidationError) {
        self.errors.push(error);
        self.valid = false;
    }

    /// Add a warning
    pub fn add_warning(&mut self, warning: ValidationWarning) {
        self.warnings.push(warning);
    }

    /// Check if validation passed
    #[must_use] 
    pub fn is_valid(&self) -> bool {
        self.valid && self.errors.is_empty()
    }

    /// Get formatted error report
    #[must_use] 
    pub fn error_report(&self) -> String {
        let mut report = String::new();

        if !self.errors.is_empty() {
            report.push_str("Validation Errors:\n");
            for error in &self.errors {
                report.push_str(&format!("  ❌ {error:?}\n"));
            }
        }

        if !self.warnings.is_empty() {
            report.push_str("Warnings:\n");
            for warning in &self.warnings {
                report.push_str(&format!("  ⚠️  {warning:?}\n"));
            }
        }

        if self.is_valid() {
            report.push_str("✅ All validations passed\n");
        }

        report
    }
}

/// Validate a package manifest and its files
///
/// # Arguments
/// * `manifest` - The parsed manifest
/// * `files` - Map of file paths to their contents
#[must_use] 
pub fn validate_package(
    manifest: &AgentManifest,
    files: &std::collections::HashMap<String, Vec<u8>>,
) -> ValidationResult {
    let mut result = ValidationResult::success();

    // Validate format version
    if manifest.agent.export_format != "1.0" {
        // Check if it's a compatible older version
        let version_parts: Vec<&str> = manifest.agent.export_format.split('.').collect();
        let major_version = version_parts.first().and_then(|v| v.parse::<u32>().ok());

        match major_version {
            Some(major) if major < 1 => {
                result.add_warning(ValidationWarning::OlderFormatVersion {
                    current: "1.0".to_string(),
                    package: manifest.agent.export_format.clone(),
                });
            }
            Some(major) if major > 1 => {
                result.add_error(ValidationError::UnsupportedFormatVersion {
                    expected: "1.0".to_string(),
                    actual: manifest.agent.export_format.clone(),
                });
            }
            _ => {
                result.add_error(ValidationError::UnsupportedFormatVersion {
                    expected: "1.0".to_string(),
                    actual: manifest.agent.export_format.clone(),
                });
            }
        }
    }

    // Validate required files exist
    let required_files = vec!["manifest.toml", "identity/did.json", "config/agent.toml"];

    for file in &required_files {
        if !files.contains_key(*file) {
            result.add_error(ValidationError::MissingFile(file.to_string()));
        } else if files.get(*file).is_none_or(std::vec::Vec::is_empty) {
            result.add_error(ValidationError::EmptyFile(file.to_string()));
        }
    }

    // Validate checksums for all listed files
    for (file_path, expected_checksum) in &manifest.packaging.checksums {
        match files.get(file_path) {
            Some(content) => {
                let actual_checksum = AgentManifest::compute_checksum(content);
                if &actual_checksum != expected_checksum {
                    result.add_error(ValidationError::ChecksumMismatch {
                        file: file_path.clone(),
                        expected: expected_checksum.clone(),
                        actual: actual_checksum,
                    });
                }
            }
            None => {
                // File in manifest but not in package
                result.add_error(ValidationError::MissingFile(file_path.clone()));
            }
        }
    }

    // Check for unknown files (not in manifest)
    for file_path in files.keys() {
        if !manifest.packaging.files.contains(file_path) {
            result.add_warning(ValidationWarning::UnknownFile(file_path.clone()));
        }
    }

    // Security warnings
    if !manifest.identity.encrypted {
        result.add_warning(ValidationWarning::UnencryptedKeys);
    }
    if !manifest.memory.encrypted {
        result.add_warning(ValidationWarning::UnencryptedMemory);
    }

    result
}

/// Quick validation without signature verification
#[must_use] 
pub fn quick_validate(files: &std::collections::HashMap<String, Vec<u8>>) -> ValidationResult {
    let manifest_bytes = match files.get("manifest.toml") {
        Some(bytes) => bytes,
        None => {
            return ValidationResult::failure(ValidationError::MissingFile(
                "manifest.toml".to_string(),
            ))
        }
    };

    let manifest_str = match std::str::from_utf8(manifest_bytes) {
        Ok(s) => s,
        Err(_) => {
            return ValidationResult::failure(ValidationError::InvalidManifest(
                "Not valid UTF-8".to_string(),
            ))
        }
    };

    let manifest = match AgentManifest::from_toml(manifest_str) {
        Ok(m) => m,
        Err(e) => {
            return ValidationResult::failure(ValidationError::InvalidManifest(e.to_string()))
        }
    };

    validate_package(&manifest, files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portable::manifest::AgentManifest;

    fn create_test_files() -> std::collections::HashMap<String, Vec<u8>> {
        let mut files = std::collections::HashMap::new();
        files.insert("manifest.toml".to_string(), b"test".to_vec());
        files.insert("identity/did.json".to_string(), b"{}".to_vec());
        files.insert("config/agent.toml".to_string(), b"test".to_vec());
        files
    }

    #[test]
    fn test_validation_success() {
        let mut manifest = AgentManifest::new("test", "1.0.0", "did:pekobot:test");
        let files = create_test_files();

        // Add files to manifest with correct checksums
        for (path, content) in &files {
            manifest.add_file(path, content);
        }

        let result = validate_package(&manifest, &files);
        assert!(result.is_valid());
    }

    #[test]
    fn test_missing_required_file() {
        let manifest = AgentManifest::new("test", "1.0.0", "did:pekobot:test");
        let mut files = create_test_files();
        files.remove("identity/did.json");

        let result = validate_package(&manifest, &files);
        assert!(!result.is_valid());
        assert!(result
            .errors
            .iter()
            .any(|e| matches!(e, ValidationError::MissingFile(_))));
    }

    #[test]
    fn test_checksum_mismatch() {
        let mut manifest = AgentManifest::new("test", "1.0.0", "did:pekobot:test");
        let mut files = create_test_files();

        manifest.add_file("config/agent.toml", b"original");
        files.insert("config/agent.toml".to_string(), b"tampered".to_vec());

        let result = validate_package(&manifest, &files);
        assert!(!result.is_valid());
        assert!(result
            .errors
            .iter()
            .any(|e| matches!(e, ValidationError::ChecksumMismatch { .. })));
    }

    #[test]
    fn test_unknown_file_warning() {
        let mut manifest = AgentManifest::new("test", "1.0.0", "did:pekobot:test");
        let mut files = create_test_files();
        files.insert("extra/file.txt".to_string(), b"extra".to_vec());

        manifest.add_file("manifest.toml", &files["manifest.toml"]);
        manifest.add_file("identity/did.json", &files["identity/did.json"]);
        manifest.add_file("config/agent.toml", &files["config/agent.toml"]);

        let result = validate_package(&manifest, &files);
        assert!(result.is_valid()); // Still valid, just warning
        assert!(result
            .warnings
            .iter()
            .any(|w| matches!(w, ValidationWarning::UnknownFile(_))));
    }

    #[test]
    fn test_unsupported_version() {
        let mut manifest = AgentManifest::new("test", "1.0.0", "did:pekobot:test");
        manifest.agent.export_format = "2.0".to_string();
        let files = create_test_files();

        let result = validate_package(&manifest, &files);
        assert!(!result.is_valid());
        assert!(result
            .errors
            .iter()
            .any(|e| matches!(e, ValidationError::UnsupportedFormatVersion { .. })));
    }
}
