//! Research Agent Example
//!
//! Demonstrates automated research with web search, content extraction,
//! credibility assessment, and report generation.
//!
//! Usage:
//!   cargo run --example research_agent -- --query "AI in healthcare"

use clap::Parser;
use std::io::{self, Write};

use pekobot::tools::research::{
    ResearchConfig, ResearchTool, SearchProvider, SearchParams, OutputFormat, CitationStyle,
};

#[derive(Parser)]
#[command(name = "research_agent")]
#[command(about = "AI-powered research assistant")]
struct Args {
    /// Research query
    #[arg(short, long)]
    query: Option<String>,

    /// Output format: markdown, html, text, json
    #[arg(short, long, default_value = "markdown")]
    format: String,

    /// Number of sources to analyze
    #[arg(short, long, default_value = "5")]
    sources: u32,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║          🔬 AI Research Agent                            ║");
    println!("║     Automated Web Research with Source Verification     ║");
    println!("╚══════════════════════════════════════════════════════════╝");
    println!();

    // Get query from user if not provided
    let query = match args.query {
        Some(q) => q,
        None => {
            print!("Enter research topic: ");
            io::stdout().flush()?;
            let mut input = String::new();
            io::stdin().read_line(&mut input)?;
            input.trim().to_string()
        }
    };

    if query.is_empty() {
        println!("❌ No query provided. Exiting.");
        return Ok(());
    }

    println!("🔍 Research Topic: {}\n", query);

    // Setup research tool (using mock config for demo)
    let config = ResearchConfig {
        search_provider: SearchProvider::Brave,
        api_key: "demo_key".to_string(), // Would use real API key
        default_params: SearchParams {
            num_results: args.sources,
            time_range: None,
            safe_search: true,
            language: Some("en".to_string()),
            region: Some("us".to_string()),
        },
        default_output_format: match args.format.as_str() {
            "html" => OutputFormat::Html,
            "text" => OutputFormat::PlainText,
            "json" => OutputFormat::Json,
            _ => OutputFormat::Markdown,
        },
        citation_style: CitationStyle::Apa,
    };

    let tool = ResearchTool::new(config)?;

    // Conduct research
    println!("Starting research... This may take a few moments.\n");

    match tool.research(&query,
        Some(&format!("Research Report: {}", query))
    ).await {
        Ok(report) => {
            println!("✅ Research complete!\n");
            println!("=".repeat(60));
            println!("📊 RESEARCH REPORT");
            println!("=".repeat(60));
            println!();

            // Display executive summary
            println!("📝 EXECUTIVE SUMMARY");
            println!("-".repeat(40));
            println!("{}", report.executive_summary);
            println!();

            // Display findings
            if !report.findings.is_empty() {
                println!("🔍 KEY FINDINGS");
                println!("-".repeat(40));
                
                for (i, finding) in report.findings.iter().enumerate() {
                    println!("\n{}. {}", i + 1, finding.topic);
                    println!("   {}", finding.summary);
                    println!("   Confidence: {:.0}%", finding.confidence * 100.0);
                    
                    if !finding.supporting_sources.is_empty() {
                        println!("   Supporting sources: {}", finding.supporting_sources.len());
                    }
                }
                println!();
            }

            // Display sources summary
            println!("📚 SOURCES ANALYZED");
            println!("-".repeat(40));
            println!("Total sources: {}", report.sources.len());
            
            let avg_credibility: f32 = report.sources.iter()
                .map(|s| s.credibility_score)
                .sum::<f32>() / report.sources.len() as f32;
            println!("Average credibility: {:.0}%", avg_credibility * 100.0);

            let high_cred = report.sources.iter()
                .filter(|s| s.credibility_score > 0.7)
                .count();
            println!("High credibility sources: {}", high_cred);
            println!();

            // Display source details
            println!("Source Details:");
            for (i, source) in report.sources.iter().enumerate() {
                let cred_icon = if source.credibility_score > 0.7 {
                    "🟢"
                } else if source.credibility_score > 0.4 {
                    "🟡"
                } else {
                    "🔴"
                };
                
                println!("  {} {}. {}", cred_icon, i + 1, source.title);
                println!("     Credibility: {:.0}% | Type: {:?}",
                    source.credibility_score * 100.0,
                    source.source_type
                );
                println!("     URL: {}", source.url);
                
                if !source.key_points.is_empty() {
                    println!("     Key points:");
                    for point in source.key_points.iter().take(3) {
                        println!("       • {}", point);
                    }
                }
                println!();
            }

            // Export report
            println!("💾 EXPORTING REPORT");
            println!("-".repeat(40));
            
            let output = tool.export_report(&report,
                match args.format.as_str() {
                    "html" => OutputFormat::Html,
                    "text" => OutputFormat::PlainText,
                    "json" => OutputFormat::Json,
                    _ => OutputFormat::Markdown,
                }
            );

            let filename = format!("research_report_{}.md", 
                chrono::Local::now().format("%Y%m%d_%H%M%S"));
            
            match std::fs::write(&filename, &output) {
                Ok(_) => println!("✅ Report saved to: {}", filename),
                Err(e) => println!("⚠️  Could not save file: {}", e),
            }

            // Print first part of report
            println!("\n📄 REPORT PREVIEW (first 1000 chars):");
            println!("{}", "-".repeat(60));
            println!("{}", &output[..output.len().min(1000)]);
            println!("{}...", &output[1000..output.len().min(1500)]);
            println!();

            // Print citations
            if !report.citations.is_empty() {
                println!("📖 CITATIONS");
                println!("-".repeat(40));
                for (i, citation) in report.citations.iter().enumerate() {
                    println!("{}. {}", i + 1, citation.formatted_citation);
                }
                println!();
            }

            // Limitations
            if !report.limitations.is_empty() {
                println!("⚠️  LIMITATIONS");
                println!("-".repeat(40));
                for limitation in &report.limitations {
                    println!("  • {}", limitation);
                }
                println!();
            }
        }
        Err(e) => {
            println!("❌ Research failed: {}", e);
            println!("\nNote: This demo requires a valid search API key.");
            println!("Supported providers: Brave, Google, Bing, Serper");
        }
    }

    println!("\n✨ Research complete!");
    println!("\n💡 Tips:");
    println!("  • Review source credibility scores before citing");
    println!("  • Verify key facts with primary sources");
    println!("  • Consider recency of sources for time-sensitive topics");

    Ok(())
}
