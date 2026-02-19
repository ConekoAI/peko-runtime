//! Document processing tool for PDFs, images, and text extraction
//!
//! Supports:
//! - PDF text extraction
//! - OCR for scanned documents
//! - Structured parsing (invoices, receipts, forms)
//! - Report generation

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;

use crate::tools::Tool;

/// Document extraction result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionResult {
    pub text: String,
    pub pages: Vec<PageContent>,
    pub metadata: DocumentMetadata,
}

/// Page content with layout info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageContent {
    pub page_number: usize,
    pub text: String,
    pub tables: Vec<Table>,
    pub images: Vec<ImageInfo>,
}

/// Table structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Table {
    pub rows: Vec<Vec<String>>,
    pub headers: Option<Vec<String>>,
}

/// Image info from document
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageInfo {
    pub description: String,
    pub position: (f64, f64),
    pub size: (f64, f64),
}

/// Document metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentMetadata {
    pub title: Option<String>,
    pub author: Option<String>,
    pub pages: usize,
    pub creation_date: Option<String>,
}

/// Parsed invoice/receipt
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Invoice {
    pub vendor: Option<String>,
    pub invoice_number: Option<String>,
    pub date: Option<String>,
    pub total_amount: Option<f64>,
    pub currency: Option<String>,
    pub line_items: Vec<LineItem>,
    pub tax_amount: Option<f64>,
}

/// Line item in invoice
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineItem {
    pub description: String,
    pub quantity: f64,
    pub unit_price: f64,
    pub total: f64,
}

/// OCR configuration
#[derive(Debug, Clone)]
pub struct OcrConfig {
    pub language: String,
    pub dpi: u32,
    pub engine: OcrEngine,
}

impl Default for OcrConfig {
    fn default() -> Self {
        Self {
            language: "eng".to_string(),
            dpi: 300,
            engine: OcrEngine::Tesseract,
        }
    }
}

/// OCR engine type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OcrEngine {
    Tesseract,
    // Future: AzureVision, GoogleVision, etc.
}

/// Document processing tool
pub struct DocumentTool {
    ocr_config: OcrConfig,
}

impl DocumentTool {
    /// Create new document tool with default config
    #[must_use] 
    pub fn new() -> Self {
        Self {
            ocr_config: OcrConfig::default(),
        }
    }

    /// Create with custom OCR config
    #[must_use] 
    pub fn with_ocr_config(mut self, config: OcrConfig) -> Self {
        self.ocr_config = config;
        self
    }

    /// Extract text from PDF
    fn extract_pdf_text(&self, file_path: &str) -> anyhow::Result<ExtractionResult> {
        // Check if file exists
        if !std::path::Path::new(file_path).exists() {
            return Err(anyhow::anyhow!("File not found: {file_path}"));
        }

        // Try to extract using external pdftotext if available
        let output = std::process::Command::new("pdftotext")
            .args(["-layout", file_path, "-"]) // - outputs to stdout
            .output();

        let text = match output {
            Ok(result) if result.status.success() => {
                String::from_utf8_lossy(&result.stdout).to_string()
            }
            _ => {
                // Fallback: try to read as binary and extract what we can
                // For now, return error with helpful message
                return Err(anyhow::anyhow!(
                    "PDF extraction failed. Install poppler-utils (pdftotext) or provide text-based PDF."
                ));
            }
        };

        // Split into pages (rough approximation by newlines or page markers)
        let pages: Vec<PageContent> = text
            .split("\n\n") // Double newline often indicates page breaks
            .enumerate()
            .filter(|(_i, content)| !content.trim().is_empty())
            .map(|(i, content)| PageContent {
                page_number: i + 1,
                text: content.trim().to_string(),
                tables: vec![], // Would need more sophisticated parsing
                images: vec![],
            })
            .collect();

        let page_count = pages.len().max(1);

        Ok(ExtractionResult {
            text: text.clone(),
            pages,
            metadata: DocumentMetadata {
                title: None, // Would need PDF metadata extraction
                author: None,
                pages: page_count,
                creation_date: None,
            },
        })
    }

    /// Extract text from image using OCR
    fn ocr_image(&self, image_path: &str) -> anyhow::Result<String> {
        // Check if file exists
        if !std::path::Path::new(image_path).exists() {
            return Err(anyhow::anyhow!("Image file not found: {image_path}"));
        }

        // Try to use tesseract if available
        let output = std::process::Command::new("tesseract")
            .args([image_path, "stdout", "-l", &self.ocr_config.language])
            .output();

        match output {
            Ok(result) if result.status.success() => {
                Ok(String::from_utf8_lossy(&result.stdout).trim().to_string())
            }
            _ => Err(anyhow::anyhow!(
                "OCR failed. Install tesseract-ocr and language packs. \
                     Ubuntu/Debian: sudo apt-get install tesseract-ocr tesseract-ocr-eng"
            )),
        }
    }

    /// Parse invoice/receipt from extracted text
    fn parse_invoice(&self, text: &str) -> Invoice {
        let lines: Vec<&str> = text.lines().collect();

        let mut invoice = Invoice {
            vendor: None,
            invoice_number: None,
            date: None,
            total_amount: None,
            currency: Some("USD".to_string()),
            line_items: vec![],
            tax_amount: None,
        };

        // Simple heuristics for invoice parsing
        for line in &lines {
            let line_lower = line.to_lowercase();

            // Look for total amount patterns
            if line_lower.contains("total") && !line_lower.contains("sub") {
                if let Some(amount) = self.extract_amount(line) {
                    invoice.total_amount = Some(amount);
                }
            }

            // Look for invoice number
            if line_lower.contains("invoice") && line_lower.contains('#') {
                if let Some(num) = self.extract_invoice_number(line) {
                    invoice.invoice_number = Some(num);
                }
            }

            // Look for date
            if line_lower.contains("date") {
                if let Some(date) = self.extract_date(line) {
                    invoice.date = Some(date);
                }
            }

            // Look for vendor (usually at top of invoice)
            if invoice.vendor.is_none()
                && !line.trim().is_empty()
                && !line_lower.starts_with("invoice")
                && !line_lower.starts_with("date")
                && !line_lower.starts_with("total")
            {
                invoice.vendor = Some(line.trim().to_string());
            }
        }

        invoice
    }

    /// Extract amount from text line
    fn extract_amount(&self, line: &str) -> Option<f64> {
        // Look for currency patterns: $100.00, 100.00 USD, etc.
        let re = regex::Regex::new(r"[\$€£]?\s*([0-9,]+\.\d{2})").ok()?;

        re.captures(line)
            .and_then(|cap| cap.get(1))
            .and_then(|m| m.as_str().replace(',', "").parse::<f64>().ok())
    }

    /// Extract invoice number
    fn extract_invoice_number(&self, line: &str) -> Option<String> {
        // Look for patterns like "Invoice #12345", "INV-12345", or "INV-ABC-123"
        let patterns = [
            r"[#:]\s*([A-Z0-9\-]+)", // Invoice #12345 or Invoice: ABC-123
            r"INV-([A-Z0-9\-]+)",    // INV-ABC-123
        ];

        for pattern in &patterns {
            if let Ok(re) = regex::Regex::new(pattern) {
                if let Some(cap) = re.captures(line) {
                    if let Some(m) = cap.get(1) {
                        return Some(m.as_str().trim().to_string());
                    }
                }
            }
        }

        None
    }

    /// Extract date from line
    fn extract_date(&self, line: &str) -> Option<String> {
        // Look for common date patterns: MM/DD/YYYY, YYYY-MM-DD, etc.
        let patterns = [
            r"(\d{1,2}/\d{1,2}/\d{2,4})",
            r"(\d{4}-\d{2}-\d{2})",
            r"(\d{1,2}-\d{1,2}-\d{2,4})",
        ];

        for pattern in &patterns {
            if let Ok(re) = regex::Regex::new(pattern) {
                if let Some(cap) = re.captures(line) {
                    if let Some(m) = cap.get(1) {
                        return Some(m.as_str().to_string());
                    }
                }
            }
        }
        None
    }

    /// Generate markdown report from extracted data
    fn generate_report(
        &self,
        title: &str,
        content: &str,
        metadata: HashMap<String, String>,
    ) -> String {
        let mut report = format!("# {title}\n\n");

        // Add metadata section
        if !metadata.is_empty() {
            report.push_str("## Metadata\n\n");
            for (key, value) in &metadata {
                report.push_str(&format!("- **{key}**: {value}\n"));
            }
            report.push('\n');
        }

        // Add content
        report.push_str("## Content\n\n");
        report.push_str(content);
        report.push('\n');

        // Add footer
        report.push_str("\n---\n\n");
        report.push_str(&format!(
            "*Generated by Pekobot Document Tool on {}*\n",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        ));

        report
    }
}

#[async_trait]
impl Tool for DocumentTool {
    fn name(&self) -> &'static str {
        "document"
    }

    fn description(&self) -> &'static str {
        r#"Document processing tool for PDFs, images, and text extraction.

Supports PDF text extraction, OCR for scanned documents, and invoice parsing.

Commands:
- extract_text: Extract text from PDF files
- ocr: Extract text from images using OCR
- parse_invoice: Extract structured data from invoices/receipts
- generate_report: Generate formatted reports from extracted data

Examples:
TOOL_CALL: {"name": "document", "parameters": {"command": "extract_text", "file_path": "/path/to/document.pdf"}}
TOOL_CALL: {"name": "document", "parameters": {"command": "ocr", "image_path": "/path/to/scanned_receipt.png"}}
TOOL_CALL: {"name": "document", "parameters": {"command": "parse_invoice", "text": "Invoice #12345..."}}
TOOL_CALL: {"name": "document", "parameters": {"command": "generate_report", "title": "Document Analysis", "content": "Extracted text here..."}}

Prerequisites:
- For PDF extraction: Install poppler-utils (pdftotext)
  Ubuntu/Debian: sudo apt-get install poppler-utils
  macOS: brew install poppler
- For OCR: Install tesseract
  Ubuntu/Debian: sudo apt-get install tesseract-ocr tesseract-ocr-eng
  macOS: brew install tesseract"#
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let command = params
            .get("command")
            .and_then(|c| c.as_str())
            .unwrap_or("extract_text");

        match command {
            "extract_text" => {
                let file_path = params
                    .get("file_path")
                    .and_then(|p| p.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'file_path' parameter"))?;

                let result = self.extract_pdf_text(file_path)?;

                Ok(json!({
                    "success": true,
                    "text": result.text,
                    "pages": result.pages.len(),
                    "metadata": {
                        "title": result.metadata.title,
                        "author": result.metadata.author,
                        "page_count": result.metadata.pages
                    },
                    "page_content": result.pages.iter().map(|p| json!({
                        "page_number": p.page_number,
                        "text_preview": &p.text[..p.text.len().min(500)]
                    })).collect::<Vec<_>>()
                }))
            }

            "ocr" => {
                let image_path = params
                    .get("image_path")
                    .and_then(|p| p.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'image_path' parameter"))?;

                let text = self.ocr_image(image_path)?;

                Ok(json!({
                    "success": true,
                    "text": text,
                    "language": self.ocr_config.language,
                    "character_count": text.len()
                }))
            }

            "parse_invoice" => {
                let text = params
                    .get("text")
                    .and_then(|t| t.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'text' parameter"))?;

                let invoice = self.parse_invoice(text);

                Ok(json!({
                    "success": true,
                    "invoice": {
                        "vendor": invoice.vendor,
                        "invoice_number": invoice.invoice_number,
                        "date": invoice.date,
                        "total_amount": invoice.total_amount,
                        "currency": invoice.currency,
                        "tax_amount": invoice.tax_amount,
                        "line_items": invoice.line_items
                    }
                }))
            }

            "generate_report" => {
                let title = params
                    .get("title")
                    .and_then(|t| t.as_str())
                    .unwrap_or("Document Report");

                let content = params
                    .get("content")
                    .and_then(|c| c.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing 'content' parameter"))?;

                let metadata: HashMap<String, String> = params
                    .get("metadata")
                    .and_then(|m| serde_json::from_value(m.clone()).ok())
                    .unwrap_or_default();

                let report = self.generate_report(title, content, metadata);

                Ok(json!({
                    "success": true,
                    "report": report,
                    "report_length": report.len()
                }))
            }

            _ => Err(anyhow::anyhow!(
                "Unknown command: {command}. Use 'extract_text', 'ocr', 'parse_invoice', or 'generate_report'"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_amount() {
        let tool = DocumentTool::new();
        assert_eq!(tool.extract_amount("Total: $100.00"), Some(100.0));
        assert_eq!(tool.extract_amount("Price: €50.50"), Some(50.5));
        assert_eq!(tool.extract_amount("1,234.56"), Some(1234.56));
    }

    #[test]
    fn test_extract_invoice_number() {
        let tool = DocumentTool::new();
        assert_eq!(
            tool.extract_invoice_number("Invoice #12345"),
            Some("12345".to_string())
        );
        assert_eq!(
            tool.extract_invoice_number("INV-ABC-123"),
            Some("ABC-123".to_string())
        );
    }

    #[test]
    fn test_parse_invoice() {
        let tool = DocumentTool::new();
        let text = r#"ACME Corporation
Invoice #INV-2024-001
Date: 2024-02-17

Consulting Services: $500.00
Total: $530.00 (including tax)"#;

        let invoice = tool.parse_invoice(text);
        assert_eq!(invoice.vendor, Some("ACME Corporation".to_string()));
        assert_eq!(invoice.invoice_number, Some("INV-2024-001".to_string()));
        assert_eq!(invoice.date, Some("2024-02-17".to_string()));
    }
}
