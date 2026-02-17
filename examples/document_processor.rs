//! Document Processor Agent Example
//!
//! A freelancer's document processing assistant that:
//! - Extracts text from PDFs
//! - Uses OCR for scanned documents
//! - Parses invoices and receipts
//! - Generates summary reports
//!
//! ## Setup
//!
//! 1. Install prerequisites:
//!    ```bash
//!    # Ubuntu/Debian
//!    sudo apt-get install poppler-utils tesseract-ocr tesseract-ocr-eng
//!
//!    # macOS
//!    brew install poppler tesseract
//!    ```
//!
//! 2. Set up your LLM provider (OpenAI, Anthropic, etc.)
//!
//! 3. Run: cargo run --example document_processor

use pekobot::agent::Agent;
use pekobot::channels::cli::{CliChannel, run_interactive_loop};
use pekobot::tools::document::DocumentTool;
use pekobot::types::agent::{AgentCapability, AgentConfig};
use pekobot::types::memory::MemoryConfig;
use pekobot::types::provider::{ProviderConfig, ProviderType, ModelConfig};
use std::collections::HashMap;
use tracing::{info, warn, error};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    info!("📄 Document Processor Agent");
    info!("==========================");
    info!("");

    // Check for required dependencies
    check_dependencies();

    // Initialize document tool
    let document_tool = DocumentTool::new();
    info!("✅ Document tool initialized");

    // Initialize AI agent
    let agent = create_document_processor_agent().await?;
    info!("✅ AI agent initialized with DID: {}", agent.did());
    info!("");
    info!("💡 Commands:");
    info!("   'process <file.pdf>' - Extract and analyze document");
    info!("   'ocr <image.png>' - Extract text from image");
    info!("   'invoice <file.pdf>' - Parse invoice data");
    info!("   'help' - Show detailed help");
    info!("");

    // Run interactive CLI
    let mut channel = CliChannel::new();
    run_document_processor_loop(agent, &mut channel, document_tool).await?;

    Ok(())
}

/// Check for required system dependencies
fn check_dependencies() {
    let mut missing = vec![];

    // Check for pdftotext
    if std::process::Command::new("pdftotext").arg("-v").output().is_err() {
        missing.push("poppler-utils (pdftotext)");
    }

    // Check for tesseract
    if std::process::Command::new("tesseract").arg("--version").output().is_err() {
        missing.push("tesseract-ocr");
    }

    if !missing.is_empty() {
        warn!("⚠️  Missing dependencies:");
        for dep in &missing {
            warn!("   - {}", dep);
        }
        warn!("");
        warn!("Some features may not work. Install with:");
        warn!("   Ubuntu/Debian: sudo apt-get install poppler-utils tesseract-ocr tesseract-ocr-eng");
        warn!("   macOS: brew install poppler tesseract");
        warn!("");
    } else {
        info!("✅ All dependencies found");
    }
}

/// Create and configure the document processor agent
async fn create_document_processor_agent() -> anyhow::Result<Agent> {
    let agent_config = AgentConfig {
        name: "document-processor".to_string(),
        description: Some("AI document analysis and processing assistant".to_string()),
        capabilities: vec![AgentCapability::Text, AgentCapability::ToolUse],
        system_prompt: Some(r#"
You are an AI document processing assistant for a freelance data analyst.

Your capabilities:
1. Extract text from PDF documents
2. OCR (text recognition) for scanned images
3. Parse invoices and receipts for structured data
4. Generate summary reports

When processing documents:
- Extract key information first
- Identify document type (invoice, contract, receipt, etc.)
- Flag any important dates, amounts, or entities
- Summarize the content in 3-5 bullet points
- Note any action items or follow-ups needed

For invoices specifically:
- Extract vendor name, invoice number, date
- Identify line items and total amount
- Note payment terms if present

Always be thorough but concise. If the document is unclear or OCR fails, say so.
"#.to_string()),
        metadata: {
            let mut m = HashMap::new();
            m.insert("role".to_string(), "document_processor".to_string());
            m.insert("version".to_string(), "1.0".to_string());
            m
        },
    };

    let memory_config = MemoryConfig {
        enabled: true,
        backend: "sqlite".to_string(),
        path: Some("./document_processor_memory.db".to_string()),
    };

    let provider_config = ProviderConfig {
        provider_type: ProviderType::OpenAI,
        model: ModelConfig {
            name: "gpt-4o-mini".to_string(),
            temperature: Some(0.3),
            max_tokens: Some(1000),
        },
        api_key: std::env::var("OPENAI_API_KEY").ok(),
        api_base: None,
    };

    let agent = Agent::new(agent_config)
        .with_memory(memory_config)
        .with_provider(provider_config)?;

    Ok(agent)
}

/// Custom interactive loop for document processing
async fn run_document_processor_loop(
    mut agent: Agent,
    channel: &mut CliChannel,
    document_tool: DocumentTool,
) -> anyhow::Result<()> {
    use std::io::{self, Write};

    println!("\n📄 Document Processor Ready!\n");

    loop {
        print!("> ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

        if input.eq_ignore_ascii_case("quit") || input.eq_ignore_ascii_case("exit") {
            println!("Goodbye!");
            break;
        }

        if input.eq_ignore_ascii_case("help") {
            print_help();
            continue;
        }

        // Parse command
        let parts: Vec<&str> = input.splitn(2, ' ').collect();
        if parts.is_empty() {
            continue;
        }

        let command = parts[0];
        let arg = parts.get(1).map(|s| *s).unwrap_or("");

        match command {
            "process" => {
                if arg.is_empty() {
                    println!("❌ Please specify a file path: process <file.pdf>");
                    continue;
                }
                process_document(&agent, &document_tool, arg).await?;
            }

            "ocr" => {
                if arg.is_empty() {
                    println!("❌ Please specify an image path: ocr <image.png>");
                    continue;
                }
                process_ocr(&document_tool, arg).await?;
            }

            "invoice" => {
                if arg.is_empty() {
                    println!("❌ Please specify a file path: invoice <file.pdf>");
                    continue;
                }
                process_invoice(&agent, &document_tool, arg).await?;
            }

            _ => {
                // Default: treat as question for AI
                match agent.execute(input).await {
                    Ok(response) => println!("\n🤖 {}\n", response),
                    Err(e) => error!("❌ Error: {}", e),
                }
            }
        }
    }

    Ok(())
}

/// Process a PDF document
async fn process_document(
    agent: &Agent,
    document_tool: &DocumentTool,
    file_path: &str,
) -> anyhow::Result<()> {
    info!("📄 Processing document: {}", file_path);

    // Extract text using document tool
    let extraction = document_tool.execute(serde_json::json!({
        "command": "extract_text",
        "file_path": file_path
    })).await?;

    if !extraction.get("success").and_then(|s| s.as_bool()).unwrap_or(false) {
        println!("❌ Failed to extract text from document");
        return Ok(());
    }

    let text = extraction
        .get("text")
        .and_then(|t| t.as_str())
        .unwrap_or("");

    let pages = extraction
        .get("pages")
        .and_then(|p| p.as_u64())
        .unwrap_or(0);

    println!("\n✅ Extracted {} pages of text", pages);

    // Use AI to analyze the content
    let analysis_prompt = format!(
        r#"Analyze this document and provide:
1. Document type (invoice, contract, receipt, etc.)
2. Key entities (names, organizations)
3. Important dates
4. Summary in 3-5 bullet points
5. Any action items or follow-ups

Document content:
{}"#,
        &text[..text.len().min(3000)] // Limit to avoid token limits
    );

    match agent.execute(&analysis_prompt).await {
        Ok(analysis) => {
            println!("\n🤖 Analysis:\n{}", analysis);
        }
        Err(e) => {
            error!("❌ Analysis failed: {}", e);
        }
    }

    // Offer to generate report
    println!("\n💡 Type 'report' to generate a formatted report");

    Ok(())
}

/// Process OCR on an image
async fn process_ocr(
    document_tool: &DocumentTool,
    image_path: &str,
) -> anyhow::Result<()> {
    info!("🔍 Running OCR on: {}", image_path);

    let result = document_tool.execute(serde_json::json!({
        "command": "ocr",
        "image_path": image_path
    })).await?;

    if !result.get("success").and_then(|s| s.as_bool()).unwrap_or(false) {
        println!("❌ OCR failed. Make sure tesseract is installed.");
        return Ok(());
    }

    let text = result
        .get("text")
        .and_then(|t| t.as_str())
        .unwrap_or("");

    let char_count = result
        .get("character_count")
        .and_then(|c| c.as_u64())
        .unwrap_or(0);

    println!("\n✅ OCR Complete ({} characters)\n", char_count);
    println!("Extracted text:\n{}", text);

    Ok(())
}

/// Process invoice/receipt
async fn process_invoice(
    agent: &Agent,
    document_tool: &DocumentTool,
    file_path: &str,
) -> anyhow::Result<()> {
    info!("🧾 Processing invoice: {}", file_path);

    // First extract text
    let extraction = document_tool.execute(serde_json::json!({
        "command": "extract_text",
        "file_path": file_path
    })).await?;

    if !extraction.get("success").and_then(|s| s.as_bool()).unwrap_or(false) {
        println!("❌ Failed to extract text from invoice");
        return Ok(());
    }

    let text = extraction
        .get("text")
        .and_then(|t| t.as_str())
        .unwrap_or("");

    // Parse invoice structure
    let parsed = document_tool.execute(serde_json::json!({
        "command": "parse_invoice",
        "text": text
    })).await?;

    println!("\n🧾 Invoice Analysis\n");

    if let Some(invoice) = parsed.get("invoice") {
        if let Some(vendor) = invoice.get("vendor").and_then(|v| v.as_str()) {
            println!("Vendor: {}", vendor);
        }
        if let Some(num) = invoice.get("invoice_number").and_then(|n| n.as_str()) {
            println!("Invoice #: {}", num);
        }
        if let Some(date) = invoice.get("date").and_then(|d| d.as_str()) {
            println!("Date: {}", date);
        }
        if let Some(total) = invoice.get("total_amount").and_then(|t| t.as_f64()) {
            let currency = invoice.get("currency").and_then(|c| c.as_str()).unwrap_or("USD");
            println!("Total: {:.2} {}", total, currency);
        }
    }

    // AI analysis for deeper insights
    let analysis_prompt = format!(
        r#"Analyze this invoice/receipt and provide:
1. Verification: Does this look like a valid invoice?
2. Red flags: Any suspicious items or inconsistencies?
3. Payment status: Any indications of paid/unpaid status?
4. Recommendations: Any follow-up actions needed?

Invoice text:
{}"#,
        &text[..text.len().min(2000)]
    );

    match agent.execute(&analysis_prompt).await {
        Ok(analysis) => {
            println!("\n🤖 AI Analysis:\n{}", analysis);
        }
        Err(e) => {
            error!("❌ Analysis failed: {}", e);
        }
    }

    Ok(())
}

/// Print help text
fn print_help() {
    println!(r#"
📄 Document Processor Commands:

  process <file.pdf>    - Extract and analyze a PDF document
  ocr <image.png>       - Extract text from an image using OCR
  invoice <file.pdf>    - Parse invoice/receipt for structured data
  help                  - Show this help message
  quit/exit             - Exit the program

Examples:
  > process contract.pdf
  > ocr scanned_receipt.png
  > invoice vendor_bill.pdf

The agent will:
- Extract text from documents
- Analyze content using AI
- Identify key information
- Generate summaries

Prerequisites:
- poppler-utils (pdftotext) for PDF processing
- tesseract-ocr for image OCR
"#);
}
