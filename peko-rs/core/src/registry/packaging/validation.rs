//! Validation result/error/warning types shared by package importers.

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
