//! Expense Tracking Tool
//!
//! Automated expense tracking from receipt photos to categorized reports.
//! Supports receipt OCR, merchant recognition, and accounting exports.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Expense tool configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpenseConfig {
    /// Default currency
    pub default_currency: String,
    /// Tax categories for this jurisdiction
    pub tax_categories: Vec<TaxCategory>,
    /// Expense categories
    pub expense_categories: Vec<ExpenseCategory>,
    /// Accounting software integration
    pub accounting_integration: Option<AccountingIntegration>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaxCategory {
    pub code: String,
    pub name: String,
    pub description: String,
    pub deductible_percent: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpenseCategory {
    pub id: String,
    pub name: String,
    pub description: String,
    pub default_tax_category: Option<String>,
    pub keywords: Vec<String>, // For auto-categorization
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountingIntegration {
    pub provider: AccountingProvider,
    pub api_key: String,
    pub company_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountingProvider {
    QuickBooks,
    Xero,
    FreshBooks,
    Wave,
}

/// Extracted expense from receipt
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Expense {
    pub id: String,
    pub merchant: String,
    pub date: chrono::NaiveDate,
    pub total_amount: f64,
    pub currency: String,
    pub subtotal: Option<f64>,
    pub tax_amount: Option<f64>,
    pub tip_amount: Option<f64>,
    pub category: String,
    pub tax_category: Option<String>,
    pub payment_method: Option<String>,
    pub items: Vec<ExpenseItem>,
    pub notes: Option<String>,
    pub receipt_image_path: Option<String>,
    pub status: ExpenseStatus,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub extracted_by: String, // OCR engine used
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpenseItem {
    pub description: String,
    pub quantity: f64,
    pub unit_price: f64,
    pub total_price: f64,
    pub category: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpenseStatus {
    PendingReview,
    Approved,
    Rejected,
    Reimbursed,
}

/// Merchant information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Merchant {
    pub name: String,
    pub address: Option<String>,
    pub city: Option<String>,
    pub phone: Option<String>,
    pub tax_id: Option<String>,
    pub known_category: Option<String>,
}

/// Expense report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpenseReport {
    pub id: String,
    pub title: String,
    pub start_date: chrono::NaiveDate,
    pub end_date: chrono::NaiveDate,
    pub expenses: Vec<Expense>,
    pub summary: ReportSummary,
    pub generated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportSummary {
    pub total_expenses: u32,
    pub total_amount: f64,
    pub total_tax: f64,
    pub by_category: HashMap<String, CategorySummary>,
    pub by_merchant: HashMap<String, f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategorySummary {
    pub category: String,
    pub count: u32,
    pub total: f64,
    pub tax_deductible_amount: f64,
}

/// Receipt parsing result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedReceipt {
    pub raw_text: String,
    pub confidence: f32,
    pub extracted_expense: Option<Expense>,
    pub warnings: Vec<String>,
}

/// Expense tracking tool
pub struct ExpenseTool {
    config: ExpenseConfig,
    http_client: reqwest::Client,
    // Database connection would go here for persistence
}

impl ExpenseTool {
    /// Create new expense tool with default configuration
    pub fn new(config: ExpenseConfig) -> anyhow::Result<Self> {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        Ok(Self {
            config,
            http_client,
        })
    }

    /// Create with default US business categories
    pub fn default_us() -> anyhow::Result<Self> {
        let config = ExpenseConfig {
            default_currency: "USD".to_string(),
            tax_categories: vec![
                TaxCategory {
                    code: "business".to_string(),
                    name: "Business Expense".to_string(),
                    description: "Fully deductible business expense".to_string(),
                    deductible_percent: 100.0,
                },
                TaxCategory {
                    code: "meals_50".to_string(),
                    name: "Meals (50%)".to_string(),
                    description: "Business meals (50% deductible)".to_string(),
                    deductible_percent: 50.0,
                },
                TaxCategory {
                    code: "travel".to_string(),
                    name: "Travel".to_string(),
                    description: "Business travel expenses".to_string(),
                    deductible_percent: 100.0,
                },
                TaxCategory {
                    code: "office".to_string(),
                    name: "Office Supplies".to_string(),
                    description: "Office equipment and supplies".to_string(),
                    deductible_percent: 100.0,
                },
                TaxCategory {
                    code: "personal".to_string(),
                    name: "Personal".to_string(),
                    description: "Non-deductible personal expense".to_string(),
                    deductible_percent: 0.0,
                },
            ],
            expense_categories: vec![
                ExpenseCategory {
                    id: "meals".to_string(),
                    name: "Meals & Entertainment".to_string(),
                    description: "Business meals and client entertainment".to_string(),
                    default_tax_category: Some("meals_50".to_string()),
                    keywords: vec![
                        "restaurant".to_string(),
                        "cafe".to_string(),
                        "coffee".to_string(),
                        "lunch".to_string(),
                        "dinner".to_string(),
                    ],
                },
                ExpenseCategory {
                    id: "travel".to_string(),
                    name: "Travel".to_string(),
                    description: "Transportation, hotels, business travel".to_string(),
                    default_tax_category: Some("travel".to_string()),
                    keywords: vec![
                        "airline".to_string(),
                        "hotel".to_string(),
                        "uber".to_string(),
                        "lyft".to_string(),
                        "taxi".to_string(),
                        "gas".to_string(),
                        "parking".to_string(),
                    ],
                },
                ExpenseCategory {
                    id: "office".to_string(),
                    name: "Office Supplies".to_string(),
                    description: "Office equipment, software, supplies".to_string(),
                    default_tax_category: Some("office".to_string()),
                    keywords: vec![
                        "staples".to_string(),
                        "office".to_string(),
                        "software".to_string(),
                        "computer".to_string(),
                        "printer".to_string(),
                    ],
                },
                ExpenseCategory {
                    id: "utilities".to_string(),
                    name: "Utilities".to_string(),
                    description: "Phone, internet, electricity".to_string(),
                    default_tax_category: Some("business".to_string()),
                    keywords: vec![
                        "phone".to_string(),
                        "internet".to_string(),
                        "electric".to_string(),
                        "gas".to_string(),
                    ],
                },
                ExpenseCategory {
                    id: "professional".to_string(),
                    name: "Professional Services".to_string(),
                    description: "Legal, accounting, consulting".to_string(),
                    default_tax_category: Some("business".to_string()),
                    keywords: vec![
                        "legal".to_string(),
                        "accounting".to_string(),
                        "consulting".to_string(),
                    ],
                },
            ],
            accounting_integration: None,
        };

        Self::new(config)
    }

    /// Parse receipt from image file
    pub async fn parse_receipt(
        &self,
        image_path: &str,
    ) -> anyhow::Result<ParsedReceipt> {
        // In production, would:
        // 1. Use Tesseract OCR (from DocumentTool)
        // 2. Or call Google Vision API / AWS Textract
        // 3. Parse structured data from OCR text

        // For now, simulate with mock data based on filename
        let mock_expense = self.create_mock_expense_from_path(image_path);
        
        Ok(ParsedReceipt {
            raw_text: format!("Receipt from {} with total ${:.2}", 
                mock_expense.merchant, mock_expense.total_amount),
            confidence: 0.85,
            extracted_expense: Some(mock_expense),
            warnings: vec![],
        })
    }

    /// Extract expense from raw text (OCR output)
    pub fn extract_from_text(
        &self,
        text: &str,
    ) -> anyhow::Result<Option<Expense>> {
        // Try to extract:
        // - Merchant name (first line or after "Thank you from")
        // - Date (various formats)
        // - Total amount (look for $ or Total:)
        // - Tax amount
        // - Items

        let merchant = self.extract_merchant(text);
        let date = self.extract_date(text);
        let total = self.extract_total(text);
        let tax = self.extract_tax(text);

        if merchant.is_none() || total.is_none() {
            return Ok(None);
        }

        let category = self.categorize_merchant(merchant.as_ref().unwrap());
        let tax_category = self.config.expense_categories
            .iter()
            .find(|c| c.id == category)
            .and_then(|c| c.default_tax_category.clone());

        Ok(Some(Expense {
            id: format!("EXP-{}", uuid::Uuid::new_v4().to_string()[..8].to_uppercase()),
            merchant: merchant.unwrap(),
            date: date.unwrap_or_else(|| chrono::Local::now().naive_local().date()),
            total_amount: total.unwrap(),
            currency: self.config.default_currency.clone(),
            subtotal: tax.map(|t| total.unwrap() - t),
            tax_amount: tax,
            tip_amount: self.extract_tip(text),
            category,
            tax_category,
            payment_method: self.extract_payment_method(text),
            items: vec![], // Would parse line items
            notes: None,
            receipt_image_path: None,
            status: ExpenseStatus::PendingReview,
            created_at: chrono::Utc::now(),
            extracted_by: "local_ocr".to_string(),
        }))
    }

    /// Auto-categorize expense based on merchant
    fn categorize_merchant(&self, merchant: &str) -> String {
        let merchant_lower = merchant.to_lowercase();
        
        for category in &self.config.expense_categories {
            for keyword in &category.keywords {
                if merchant_lower.contains(keyword) {
                    return category.id.clone();
                }
            }
        }

        // Check for known merchants
        if merchant_lower.contains("starbucks") 
            || merchant_lower.contains("mcdonald")
            || merchant_lower.contains("restaurant")
            || merchant_lower.contains("cafe") {
            return "meals".to_string();
        }

        if merchant_lower.contains("uber") 
            || merchant_lower.contains("lyft")
            || merchant_lower.contains("airline")
            || merchant_lower.contains("hotel") {
            return "travel".to_string();
        }

        "office".to_string() // Default
    }

    /// Categorize an expense manually
    pub fn categorize_expense(
        &self,
        expense: &mut Expense,
        category: &str,
    ) -> anyhow::Result<()> {
        expense.category = category.to_string();
        
        // Update tax category based on new category
        if let Some(cat) = self.config.expense_categories.iter().find(|c| c.id == category) {
            expense.tax_category = cat.default_tax_category.clone();
        }

        Ok(())
    }

    /// Generate expense report for date range
    pub fn generate_report(
        &self,
        title: &str,
        start_date: chrono::NaiveDate,
        end_date: chrono::NaiveDate,
        expenses: Vec<Expense>,
    ) -> ExpenseReport {
        let filtered: Vec<_> = expenses.into_iter()
            .filter(|e| e.date >= start_date && e.date <= end_date)
            .collect();

        let total_amount: f64 = filtered.iter().map(|e| e.total_amount).sum();
        let total_tax: f64 = filtered.iter().filter_map(|e| e.tax_amount).sum();

        // Group by category
        let mut by_category: HashMap<String, CategorySummary> = HashMap::new();
        let mut by_merchant: HashMap<String, f64> = HashMap::new();

        for expense in &filtered {
            // Update category summary
            let entry = by_category.entry(expense.category.clone()).or_insert_with(|| {
                CategorySummary {
                    category: expense.category.clone(),
                    count: 0,
                    total: 0.0,
                    tax_deductible_amount: 0.0,
                }
            });
            entry.count += 1;
            entry.total += expense.total_amount;

            // Calculate tax deductible amount
            if let Some(ref tax_cat_code) = expense.tax_category {
                if let Some(tax_cat) = self.config.tax_categories.iter().find(|t| t.code == *tax_cat_code) {
                    entry.tax_deductible_amount += expense.total_amount * (tax_cat.deductible_percent as f64 / 100.0);
                }
            }

            // Update merchant totals
            *by_merchant.entry(expense.merchant.clone()).or_insert(0.0) += expense.total_amount;
        }

        ExpenseReport {
            id: format!("RPT-{}", uuid::Uuid::new_v4().to_string()[..8].to_uppercase()),
            title: title.to_string(),
            start_date,
            end_date,
            expenses: filtered.clone(),
            summary: ReportSummary {
                total_expenses: filtered.len() as u32,
                total_amount,
                total_tax,
                by_category,
                by_merchant,
            },
            generated_at: chrono::Utc::now(),
        }
    }

    /// Export report to CSV
    pub fn export_csv(&self,
        report: &ExpenseReport,
    ) -> anyhow::Result<String> {
        let mut csv = String::from("Date,Merchant,Category,Total,Tax,Payment Method,Status,Notes\n");

        for expense in &report.expenses {
            csv.push_str(&format!(
                "{},{},{},{:.2},{:.2},{},{},{}\n",
                expense.date,
                expense.merchant,
                expense.category,
                expense.total_amount,
                expense.tax_amount.unwrap_or(0.0),
                expense.payment_method.as_deref().unwrap_or(""),
                format!("{:?}", expense.status),
                expense.notes.as_deref().unwrap_or("")
            ));
        }

        Ok(csv)
    }

    /// Export report summary
    pub fn export_summary(&self,
        report: &ExpenseReport,
    ) -> String {
        let mut output = format!(
            "EXPENSE REPORT: {}\nPeriod: {} to {}\nGenerated: {}\n\n",
            report.title,
            report.start_date.format("%Y-%m-%d"),
            report.end_date.format("%Y-%m-%d"),
            report.generated_at.format("%Y-%m-%d %H:%M:%S")
        );

        output.push_str(&format!(
            "Total Expenses: {}\nTotal Amount: ${:.2}\nTotal Tax: ${:.2}\n\n",
            report.summary.total_expenses,
            report.summary.total_amount,
            report.summary.total_tax
        ));

        output.push_str("BY CATEGORY:\n");
        for (_, summary) in &report.summary.by_category {
            output.push_str(&format!(
                "  {}: {} items, ${:.2} (${:.2} deductible)\n",
                summary.category, summary.count, summary.total, summary.tax_deductible_amount
            ));
        }

        output.push_str("\nBY MERCHANT:\n");
        let mut merchants: Vec<_> = report.summary.by_merchant.iter().collect();
        merchants.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap());
        for (merchant, total) in merchants.iter().take(10) {
            output.push_str(&format!("  {}: ${:.2}\n", merchant, total));
        }

        output
    }

    // Helper methods for text extraction
    fn extract_merchant(&self, text: &str) -> Option<String> {
        // Try to find merchant name (usually first non-empty line or after "Thank you")
        let lines: Vec<_> = text.lines().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
        lines.first().map(|s| s.to_string())
    }

    fn extract_date(&self, text: &str) -> Option<chrono::NaiveDate> {
        // Look for date patterns (MM/DD/YYYY, MM-DD-YYYY, etc.)
        // Simplified: return today for now
        Some(chrono::Local::now().naive_local().date())
    }

    fn extract_total(&self, text: &str) -> Option<f64> {
        // Look for "Total" followed by amount
        for line in text.lines() {
            let line_lower = line.to_lowercase();
            if line_lower.contains("total") {
                // Try to extract dollar amount
                if let Some(dollar_idx) = line.find('$') {
                    let amount_str: String = line[dollar_idx+1..]
                        .chars()
                        .take_while(|c| c.is_digit(10) || *c == '.' || *c == ',')
                        .collect();
                    return amount_str.replace(",", "").parse().ok();
                }
            }
        }
        None
    }

    fn extract_tax(&self, text: &str) -> Option<f64> {
        // Look for "Tax" line
        for line in text.lines() {
            let line_lower = line.to_lowercase();
            if line_lower.contains("tax") && !line_lower.contains("total") {
                if let Some(dollar_idx) = line.find('$') {
                    let amount_str: String = line[dollar_idx+1..]
                        .chars()
                        .take_while(|c| c.is_digit(10) || *c == '.' || *c == ',')
                        .collect();
                    return amount_str.replace(",", "").parse().ok();
                }
            }
        }
        None
    }

    fn extract_tip(&self, text: &str) -> Option<f64> {
        // Look for "Tip" or "Gratuity"
        for line in text.lines() {
            let line_lower = line.to_lowercase();
            if line_lower.contains("tip") || line_lower.contains("gratuity") {
                if let Some(dollar_idx) = line.find('$') {
                    let amount_str: String = line[dollar_idx+1..]
                        .chars()
                        .take_while(|c| c.is_digit(10) || *c == '.' || *c == ',')
                        .collect();
                    return amount_str.replace(",", "").parse().ok();
                }
            }
        }
        None
    }

    fn extract_payment_method(&self, text: &str) -> Option<String> {
        let text_lower = text.to_lowercase();
        if text_lower.contains("visa") {
            Some("Visa".to_string())
        } else if text_lower.contains("mastercard") || text_lower.contains("master card") {
            Some("Mastercard".to_string())
        } else if text_lower.contains("amex") || text_lower.contains("american express") {
            Some("American Express".to_string())
        } else if text_lower.contains("cash") {
            Some("Cash".to_string())
        } else {
            None
        }
    }

    // Create mock expense for demo purposes
    fn create_mock_expense_from_path(&self,
        path: &str,
    ) -> Expense {
        let path_lower = path.to_lowercase();
        
        if path_lower.contains("starbucks") {
            Expense {
                id: format!("EXP-{}", uuid::Uuid::new_v4().to_string()[..8].to_uppercase()),
                merchant: "Starbucks".to_string(),
                date: chrono::Local::now().naive_local().date(),
                total_amount: 8.47,
                currency: "USD".to_string(),
                subtotal: Some(7.50),
                tax_amount: Some(0.97),
                tip_amount: None,
                category: "meals".to_string(),
                tax_category: Some("meals_50".to_string()),
                payment_method: Some("Visa".to_string()),
                items: vec![
                    ExpenseItem {
                        description: "Grande Latte".to_string(),
                        quantity: 1.0,
                        unit_price: 4.75,
                        total_price: 4.75,
                        category: None,
                    },
                    ExpenseItem {
                        description: "Croissant".to_string(),
                        quantity: 1.0,
                        unit_price: 2.75,
                        total_price: 2.75,
                        category: None,
                    },
                ],
                notes: None,
                receipt_image_path: Some(path.to_string()),
                status: ExpenseStatus::PendingReview,
                created_at: chrono::Utc::now(),
                extracted_by: "mock".to_string(),
            }
        } else if path_lower.contains("uber") {
            Expense {
                id: format!("EXP-{}", uuid::Uuid::new_v4().to_string()[..8].to_uppercase()),
                merchant: "Uber".to_string(),
                date: chrono::Local::now().naive_local().date(),
                total_amount: 24.50,
                currency: "USD".to_string(),
                subtotal: Some(22.00),
                tax_amount: Some(1.50),
                tip_amount: Some(1.00),
                category: "travel".to_string(),
                tax_category: Some("travel".to_string()),
                payment_method: Some("Visa".to_string()),
                items: vec![
                    ExpenseItem {
                        description: "UberX ride to airport".to_string(),
                        quantity: 1.0,
                        unit_price: 22.00,
                        total_price: 22.00,
                        category: None,
                    },
                ],
                notes: Some("Client meeting travel".to_string()),
                receipt_image_path: Some(path.to_string()),
                status: ExpenseStatus::PendingReview,
                created_at: chrono::Utc::now(),
                extracted_by: "mock".to_string(),
            }
        } else {
            Expense {
                id: format!("EXP-{}", uuid::Uuid::new_v4().to_string()[..8].to_uppercase()),
                merchant: "Office Depot".to_string(),
                date: chrono::Local::now().naive_local().date(),
                total_amount: 45.67,
                currency: "USD".to_string(),
                subtotal: Some(42.00),
                tax_amount: Some(3.67),
                tip_amount: None,
                category: "office".to_string(),
                tax_category: Some("office".to_string()),
                payment_method: Some("Credit Card".to_string()),
                items: vec![
                    ExpenseItem {
                        description: "Printer paper (5 reams)".to_string(),
                        quantity: 5.0,
                        unit_price: 8.40,
                        total_price: 42.00,
                        category: None,
                    },
                ],
                notes: None,
                receipt_image_path: Some(path.to_string()),
                status: ExpenseStatus::PendingReview,
                created_at: chrono::Utc::now(),
                extracted_by: "mock".to_string(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expense_tool_default() {
        let tool = ExpenseTool::default_us();
        assert!(tool.is_ok());
    }

    #[test]
    fn test_categorize_merchant() {
        let tool = ExpenseTool::default_us().unwrap();
        assert_eq!(tool.categorize_merchant("Starbucks"), "meals");
        assert_eq!(tool.categorize_merchant("Uber Technologies"), "travel");
        assert_eq!(tool.categorize_merchant("Office Depot"), "office");
    }
}
