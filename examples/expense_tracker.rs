//! Expense Tracker Example
//!
//! Demonstrates automated expense tracking from receipt photos
//! to categorized reports for small business accounting.
//!
//! Usage:
//!   cargo run --example expense_tracker -- --mode scan

use chrono::{Local, NaiveDate};
use clap::Parser;
use std::io::{self, Write};

use pekobot::tools::expense::{
    ExpenseCategory, ExpenseConfig, ExpenseStatus, ExpenseTool, TaxCategory,
};

#[derive(Parser)]
#[command(name = "expense_tracker")]
#[command(about = "AI-powered expense tracking for small business")]
struct Args {
    /// Mode: scan, report, export
    #[arg(short, long, default_value = "scan")]
    mode: String,

    /// Start date for report (YYYY-MM-DD)
    #[arg(short, long)]
    from: Option<String>,

    /// End date for report (YYYY-MM-DD)
    #[arg(short, long)]
    to: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║          💼 Smart Expense Tracker                        ║");
    println!("║     AI-Powered Receipt Processing for Small Business    ║");
    println!("╚══════════════════════════════════════════════════════════╝");
    println!();

    let tool = ExpenseTool::default_us()?;
    let mut expenses: Vec<pekobot::tools::expense::Expense> = vec![];

    match args.mode.as_str() {
        "scan" => {
            println!("📸 Receipt Scan Mode\n");
            println!("Instructions:");
            println!("1. Place receipt photo files in ./receipts/ directory");
            println!("2. Or enter file paths below (one per line)");
            println!("3. Type 'done' when finished, or 'demo' for sample data");
            println!();

            loop {
                print!("Receipt path (or 'done'/'demo'): ");
                io::stdout().flush()?;

                let mut input = String::new();
                io::stdin().read_line(&mut input)?;
                let path = input.trim();

                if path == "done" {
                    break;
                }

                if path == "demo" {
                    // Add sample expenses
                    let demo_receipts = vec![
                        "./receipts/starbucks_001.jpg",
                        "./receipts/uber_002.jpg",
                        "./receipts/office_depot_003.jpg",
                        "./receipts/restaurant_004.jpg",
                    ];

                    for receipt_path in demo_receipts {
                        println!("\n📄 Processing: {}", receipt_path);

                        match tool.parse_receipt(receipt_path).await {
                            Ok(result) => {
                                println!("   Confidence: {:.0}%", result.confidence * 100.0);

                                if let Some(expense) = result.extracted_expense {
                                    println!("   Merchant: {}", expense.merchant);
                                    println!("   Date: {}", expense.date);
                                    println!("   Total: ${:.2}", expense.total_amount);
                                    println!("   Category: {}", expense.category);

                                    if !result.warnings.is_empty() {
                                        println!("   ⚠️  Warnings: {:?}", result.warnings);
                                    }

                                    print!("   ✅ Add to expenses? (y/n): ");
                                    io::stdout().flush()?;

                                    let mut confirm = String::new();
                                    io::stdin().read_line(&mut confirm)?;

                                    if confirm.trim().to_lowercase() == "y" {
                                        expenses.push(expense);
                                        println!("   Added!\n");
                                    }
                                }
                            }
                            Err(e) => {
                                println!("   ❌ Error: {}\n", e);
                            }
                        }
                    }
                    break;
                }

                // Process single receipt
                println!("\n📄 Processing: {}", path);

                match tool.parse_receipt(path).await {
                    Ok(result) => {
                        println!("   Raw text: {}", result.raw_text);
                        println!("   Confidence: {:.0}%", result.confidence * 100.0);

                        if let Some(expense) = result.extracted_expense {
                            println!("\n   💡 Extracted:");
                            println!("   Merchant: {}", expense.merchant);
                            println!("   Date: {}", expense.date);
                            println!("   Total: ${:.2}", expense.total_amount);
                            println!("   Category: {}", expense.category);
                            println!("   Tax Category: {:?}", expense.tax_category);

                            if !result.warnings.is_empty() {
                                println!("   ⚠️  Warnings: {:?}", result.warnings);
                            }

                            print!("\n   ✅ Add to expenses? (y/n): ");
                            io::stdout().flush()?;

                            let mut confirm = String::new();
                            io::stdin().read_line(&mut confirm)?;

                            if confirm.trim().to_lowercase() == "y" {
                                expenses.push(expense);
                                println!("   Added!\n");
                            } else {
                                println!("   Skipped.\n");
                            }
                        } else {
                            println!("   ⚠️  Could not extract expense data\n");
                        }
                    }
                    Err(e) => {
                        println!("   ❌ Error: {}\n", e);
                    }
                }
            }
        }

        "report" => {
            println!("📊 Expense Report Mode\n");

            // For demo, use sample data
            expenses = generate_sample_expenses();

            let from_date = args
                .from
                .and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok())
                .unwrap_or_else(|| Local::now().naive_local().date() - chrono::Duration::days(30));

            let to_date = args
                .to
                .and_then(|s| NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok())
                .unwrap_or_else(|| Local::now().naive_local().date());

            println!("Generating report from {} to {}...\n", from_date, to_date);

            let report = tool.generate_report(
                &format!("Expense Report {} - {}", from_date, to_date),
                from_date,
                to_date,
                expenses.clone(),
            );

            println!("{}", tool.export_summary(&report));
        }

        "export" => {
            println!("📁 Export Mode\n");

            expenses = generate_sample_expenses();

            let from_date = Local::now().naive_local().date() - chrono::Duration::days(30);
            let to_date = Local::now().naive_local().date();

            let report = tool.generate_report(
                "Monthly Expense Export",
                from_date,
                to_date,
                expenses.clone(),
            );

            match tool.export_csv(&report) {
                Ok(csv) => {
                    println!("CSV Export:");
                    println!("{}", csv);
                }
                Err(e) => {
                    println!("❌ Export failed: {}", e);
                }
            }
        }

        _ => {
            println!("❌ Unknown mode. Use: scan, report, or export");
        }
    }

    // Summary
    if !expenses.is_empty() {
        println!("\n📋 Session Summary:");
        println!("Total expenses added: {}", expenses.len());

        let total: f64 = expenses.iter().map(|e| e.total_amount).sum();
        println!("Total amount: ${:.2}", total);

        println!("\n💡 Tips:");
        println!("- Review pending expenses before month-end");
        println!("- Attach digital copies of receipts for audit");
        println!("- Export to your accounting software regularly");
    }

    println!("\n✨ Done!");

    Ok(())
}

fn generate_sample_expenses() -> Vec<pekobot::tools::expense::Expense> {
    use pekobot::tools::expense::{Expense, ExpenseItem, ExpenseStatus};

    vec![
        Expense {
            id: "EXP-001".to_string(),
            merchant: "Starbucks".to_string(),
            date: Local::now().naive_local().date() - chrono::Duration::days(5),
            total_amount: 8.47,
            currency: "USD".to_string(),
            subtotal: Some(7.50),
            tax_amount: Some(0.97),
            tip_amount: None,
            category: "meals".to_string(),
            tax_category: Some("meals_50".to_string()),
            payment_method: Some("Visa".to_string()),
            items: vec![ExpenseItem {
                description: "Coffee".to_string(),
                quantity: 1.0,
                unit_price: 4.75,
                total_price: 4.75,
                category: None,
            }],
            notes: Some("Client meeting".to_string()),
            receipt_image_path: None,
            status: ExpenseStatus::Approved,
            created_at: chrono::Utc::now(),
            extracted_by: "demo".to_string(),
        },
        Expense {
            id: "EXP-002".to_string(),
            merchant: "Uber".to_string(),
            date: Local::now().naive_local().date() - chrono::Duration::days(3),
            total_amount: 24.50,
            currency: "USD".to_string(),
            subtotal: Some(22.00),
            tax_amount: Some(1.50),
            tip_amount: Some(1.00),
            category: "travel".to_string(),
            tax_category: Some("travel".to_string()),
            payment_method: Some("Visa".to_string()),
            items: vec![ExpenseItem {
                description: "Airport ride".to_string(),
                quantity: 1.0,
                unit_price: 22.00,
                total_price: 22.00,
                category: None,
            }],
            notes: None,
            receipt_image_path: None,
            status: ExpenseStatus::Approved,
            created_at: chrono::Utc::now(),
            extracted_by: "demo".to_string(),
        },
        Expense {
            id: "EXP-003".to_string(),
            merchant: "Office Depot".to_string(),
            date: Local::now().naive_local().date() - chrono::Duration::days(1),
            total_amount: 45.67,
            currency: "USD".to_string(),
            subtotal: Some(42.00),
            tax_amount: Some(3.67),
            tip_amount: None,
            category: "office".to_string(),
            tax_category: Some("office".to_string()),
            payment_method: Some("Credit Card".to_string()),
            items: vec![ExpenseItem {
                description: "Printer supplies".to_string(),
                quantity: 1.0,
                unit_price: 42.00,
                total_price: 42.00,
                category: None,
            }],
            notes: None,
            receipt_image_path: None,
            status: ExpenseStatus::PendingReview,
            created_at: chrono::Utc::now(),
            extracted_by: "demo".to_string(),
        },
    ]
}
