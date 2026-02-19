//! Inventory Alerts Example
//!
//! Demonstrates automated low-stock monitoring and reorder suggestions
//! for e-commerce store owners.
//!
//! Usage:
//!   cargo run --example inventory_alerts -- --platform shopify --store yourstore.myshopify.com

use clap::Parser;
use pekobot::tools::inventory::{
    EcommercePlatform, InventoryConfig, InventoryTool, PlatformCredentials,
};
use std::io::{self, Write};

#[derive(Parser)]
#[command(name = "inventory_alerts")]
#[command(about = "E-commerce inventory monitoring and alerts")]
struct Args {
    /// Platform type (shopify or woocommerce)
    #[arg(short, long, default_value = "shopify")]
    platform: String,

    /// Store URL
    #[arg(short, long)]
    store: String,

    /// API key
    #[arg(short, long)]
    api_key: Option<String>,

    /// Low stock threshold
    #[arg(short, long, default_value = "10")]
    threshold: u32,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║          📦 Inventory Alerts Manager                      ║");
    println!("║     Automated Low-Stock Monitoring for E-commerce        ║");
    println!("╚══════════════════════════════════════════════════════════╝");
    println!();

    // Get API credentials
    let api_key = match args.api_key {
        Some(key) => key,
        None => {
            print!("Enter API key: ");
            io::stdout().flush()?;
            let mut input = String::new();
            io::stdin().read_line(&mut input)?;
            input.trim().to_string()
        }
    };

    let platform = match args.platform.as_str() {
        "shopify" => EcommercePlatform::Shopify,
        "woocommerce" => EcommercePlatform::WooCommerce,
        _ => {
            println!("❌ Unsupported platform. Use 'shopify' or 'woocommerce'");
            return Ok(());
        }
    };

    // Configure inventory tool
    let config = InventoryConfig {
        platform,
        store_url: args.store,
        credentials: PlatformCredentials {
            api_key,
            api_secret: None,
            password: None,
        },
        default_threshold: args.threshold,
        check_interval_minutes: 60,
    };

    let tool = InventoryTool::new(config)?;

    println!("\n🔍 Checking inventory...\n");

    // Check for low stock
    match tool.check_low_stock().await {
        Ok(alerts) => {
            if alerts.is_empty() {
                println!("✅ All products are well-stocked!");
            } else {
                println!("⚠️  Found {} low stock item(s):\n", alerts.len());

                for (i, alert) in alerts.iter().enumerate() {
                    let icon = match alert.severity {
                        pekobot::tools::inventory::AlertSeverity::OutOfStock => "🔴",
                        pekobot::tools::inventory::AlertSeverity::Critical => "🟠",
                        pekobot::tools::inventory::AlertSeverity::Warning => "🟡",
                    };

                    println!("{}. {} {}", i + 1, icon, alert.product.name);
                    println!("   SKU: {}", alert.product.sku);
                    println!(
                        "   Stock: {} available ({} reserved)",
                        alert.product.available_stock, alert.product.reserved_stock
                    );
                    println!("   Threshold: {}", alert.product.low_stock_threshold);
                    println!("   Action: {}", alert.suggested_action);
                    if let Some(date) = alert.estimated_stockout_date {
                        println!("   Est. stockout: {}", date.format("%Y-%m-%d"));
                    }
                    println!();
                }
            }
        }
        Err(e) => {
            println!("❌ Error checking stock: {}", e);
        }
    }

    // Generate reorder suggestions
    println!("\n📋 Reorder Suggestions:\n");

    match tool.suggest_reorders().await {
        Ok(suggestions) => {
            if suggestions.is_empty() {
                println!("No reorders needed at this time.");
            } else {
                for (i, suggestion) in suggestions.iter().enumerate() {
                    let icon = match suggestion.urgency {
                        pekobot::tools::inventory::ReorderUrgency::Critical => "🚨",
                        pekobot::tools::inventory::ReorderUrgency::High => "🔴",
                        pekobot::tools::inventory::ReorderUrgency::Medium => "🟡",
                        pekobot::tools::inventory::ReorderUrgency::Low => "🟢",
                    };

                    println!("{}. {} {}", i + 1, icon, suggestion.product_name);
                    println!("   SKU: {}", suggestion.sku);
                    println!("   Current: {} units", suggestion.current_stock);
                    println!(
                        "   Suggested order: {} units",
                        suggestion.suggested_quantity
                    );

                    if let Some(ref supplier) = suggestion.supplier {
                        println!("   Supplier: {}", supplier.name);
                        if let Some(ref email) = supplier.contact_email {
                            println!("   Contact: {}", email);
                        }
                        println!("   Lead time: {} days", supplier.lead_time_days);
                    }

                    if let Some(cost) = suggestion.total_cost {
                        println!("   Est. cost: ${:.2}", cost);
                    }

                    println!("   Reason: {}", suggestion.reason);
                    println!();
                }

                // Ask if user wants to create purchase orders
                print!("\nWould you like to create purchase orders? (y/n): ");
                io::stdout().flush()?;
                let mut input = String::new();
                io::stdin().read_line(&mut input)?;

                if input.trim().to_lowercase() == "y" {
                    for suggestion in &suggestions {
                        if let Some(ref supplier) = suggestion.supplier {
                            let items = vec![pekobot::tools::inventory::OrderItem {
                                product_id: suggestion.product_id.clone(),
                                sku: suggestion.sku.clone(),
                                product_name: suggestion.product_name.clone(),
                                quantity: suggestion.suggested_quantity,
                                unit_price: suggestion.unit_cost,
                            }];

                            match tool.create_purchase_order(supplier, items).await {
                                Ok(order) => {
                                    println!(
                                        "✅ Created purchase order {} for {} ({} units)",
                                        order.id,
                                        suggestion.product_name,
                                        suggestion.suggested_quantity
                                    );
                                    println!(
                                        "   Expected delivery: {}",
                                        order
                                            .expected_delivery
                                            .map(|d| d.format("%Y-%m-%d").to_string())
                                            .unwrap_or_else(|| "TBD".to_string())
                                    );
                                }
                                Err(e) => {
                                    println!(
                                        "❌ Failed to create order for {}: {}",
                                        suggestion.product_name, e
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        Err(e) => {
            println!("❌ Error generating suggestions: {}", e);
        }
    }

    // Show full inventory summary
    println!("\n📊 Full Inventory Summary:\n");

    match tool.list_inventory().await {
        Ok(products) => {
            println!("Total products: {}", products.len());

            let low_stock_count = products
                .iter()
                .filter(|p| (p.available_stock as u32) < p.low_stock_threshold)
                .count();

            let out_of_stock_count = products.iter().filter(|p| p.available_stock <= 0).count();

            let total_value: i32 = products.iter().map(|p| p.current_stock).sum();

            println!("Low stock items: {}", low_stock_count);
            println!("Out of stock: {}", out_of_stock_count);
            println!("Total units in stock: {}", total_value);
        }
        Err(e) => {
            println!("❌ Error listing inventory: {}", e);
        }
    }

    println!("\n✨ Inventory check complete!");

    Ok(())
}
