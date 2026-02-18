//! Email Assistant Example
//!
//! Demonstrates AI-powered email management with inbox summarization,
//! smart replies, and scheduled sending.
//!
//! Usage:
//!   cargo run --example email_assistant -- --mode summary

use clap::Parser;
use std::io::{self, Write};

use pekobot::tools::email::{
    EmailConfig, EmailTool, EmailProvider, EmailCredentials, ReplySettings, ReplyTone,
};

#[derive(Parser)]
#[command(name = "email_assistant")]
#[command(about = "AI-powered email management")]
struct Args {
    /// Mode: summary, reply, send, schedule
    #[arg(short, long, default_value = "summary")]
    mode: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║          📧 Smart Email Assistant                        ║");
    println!("║     AI-Powered Inbox Management for Professionals       ║");
    println!("╚══════════════════════════════════════════════════════════╝");
    println!();

    // Setup with mock config (in production would use real OAuth)
    let config = EmailConfig {
        provider: EmailProvider::Gmail,
        email_address: "user@example.com".to_string(),
        credentials: EmailCredentials {
            access_token: "mock_token".to_string(),
            refresh_token: None,
            password: None,
            app_password: None,
        },
        reply_settings: ReplySettings {
            default_tone: ReplyTone::Professional,
            include_signature: true,
            signature: Some("Best regards,\nYour Name".to_string()),
            auto_draft_flagged: true,
        },
    };

    let tool = EmailTool::new(config)?;

    match args.mode.as_str() {
        "summary" => {
            println!("📊 Generating inbox summary...\n");

            match tool.get_inbox_summary().await {
                Ok(summary) => {
                    println!("📨 INBOX SUMMARY");
                    println!("Generated: {}\n", summary.generated_at.format("%Y-%m-%d %H:%M"));

                    println!("📈 Overview:");
                    println!("  Total unread: {}", summary.total_unread);
                    println!("  Total threads: {}\n", summary.total_threads);

                    if !summary.categories.is_empty() {
                        println!("📁 Categories:");
                        for cat in &summary.categories {
                            println!("  {}: {} emails", cat.name, cat.count);
                        }
                        println!();
                    }

                    if !summary.urgent_emails.is_empty() {
                        println!("🔴 URGENT ({} emails):\n", summary.urgent_emails.len());
                        for (i, email) in summary.urgent_emails.iter().enumerate() {
                            println!("  {}. {}", i + 1, email.subject);
                            println!("     From: {}", 
                                email.from.name.as_ref().unwrap_or(&email.from.email));
                            println!("     Preview: {}...", 
                                &email.preview_text[..email.preview_text.len().min(60)]);
                            println!("     Urgency: {:.0}%", email.urgency_score * 100.0);
                            println!("     Action: {:?}\n", email.suggested_action);
                        }
                    }

                    if !summary.requires_reply.is_empty() {
                        println!("💬 REQUIRES REPLY ({} emails):\n", summary.requires_reply.len());
                        for (i, email) in summary.requires_reply.iter().enumerate().take(5) {
                            println!("  {}. {}", i + 1, email.subject);
                            println!("     From: {}",
                                email.from.name.as_ref().unwrap_or(&email.from.email));
                            println!("     Urgency: {:.0}%\n", email.urgency_score * 100.0);
                        }
                    }

                    if !summary.newsletters.is_empty() {
                        println!("📰 NEWSLETTERS ({} emails):", summary.newsletters.len());
                        println!("   Consider bulk archiving or unsubscribing.\n");
                    }

                    println!("💡 Suggested Actions:");
                    println!("  1. Review urgent emails first");
                    println!("  2. Draft replies to items marked 'requires reply'");
                    println!("  3. Archive newsletters if not needed");
                    println!("  4. Set up filters for recurring email types");
                }
                Err(e) => {
                    println!("❌ Error generating summary: {}", e);
                }
            }
        }

        "reply" => {
            println!("💬 Smart Reply Mode\n");
            println!("Enter an email ID to draft a reply for:");

            let mut email_id = String::new();
            io::stdin().read_line(&mut email_id)?;
            let email_id = email_id.trim();

            println!("\nSelect tone:");
            println!("  1. Professional");
            println!("  2. Friendly");
            println!("  3. Formal");
            println!("  4. Brief");
            print!("Choice: ");
            io::stdout().flush()?;

            let mut tone_choice = String::new();
            io::stdin().read_line(&mut tone_choice)?;

            let tone = match tone_choice.trim() {
                "2" => Some(ReplyTone::Friendly),
                "3" => Some(ReplyTone::Formal),
                "4" => Some(ReplyTone::Brief),
                _ => Some(ReplyTone::Professional),
            };

            println!("\n✍️  Drafting reply...\n");

            match tool.draft_reply(email_id, tone).await {
                Ok(reply) => {
                    println!("📝 DRAFT REPLY");
                    println!("==============");
                    println!("Subject: {}", reply.suggested_subject);
                    println!("Tone: {:?}", reply.tone);
                    println!("Confidence: {:.0}%\n", reply.confidence * 100.0);

                    if !reply.key_points_addressed.is_empty() {
                        println!("Key points detected:");
                        for point in &reply.key_points_addressed {
                            println!("  • {}", point);
                        }
                        println!();
                    }

                    println!("---");
                    println!("{}", reply.draft_body);
                    println!("---\n");

                    println!("Options:");
                    println!("  [e] Edit draft");
                    println!("  [s] Send now");
                    println!("  [q] Queue for later");
                    println!("  [d] Discard");
                }
                Err(e) => {
                    println!("❌ Error drafting reply: {}", e);
                }
            }
        }

        "send" => {
            println!("📤 Send Email Mode");
            println!("(Mock mode - no actual email sent)\n");

            println!("This would compose and send an email immediately.");
            println!("In production, you would:");
            println!("  1. Enter recipient");
            println!("  2. Enter subject");
            println!("  3. Compose body (with AI assistance)");
            println!("  4. Review and send");
        }

        "schedule" => {
            println!("⏰ Schedule Email Mode");
            println!("(Mock mode - no actual scheduling)\n");

            println!("This would schedule an email for optimal delivery time.");
            println!("Features:");
            println!("  • Suggest best time based on recipient timezone");
            println!("  • Consider business hours");
            println!("  • Avoid weekends/holidays");
            println!("  • Queue for batch sending");
        }

        _ => {
            println!("❌ Unknown mode. Use: summary, reply, send, or schedule");
        }
    }

    println!("\n✨ Done!");

    Ok(())
}
