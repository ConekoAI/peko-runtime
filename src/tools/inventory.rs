//! Inventory Management Tool
//!
//! E-commerce inventory monitoring with low-stock alerts and reorder automation.
//! Supports Shopify and WooCommerce APIs.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Inventory tool configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryConfig {
    /// Platform type
    pub platform: EcommercePlatform,
    /// Store URL
    pub store_url: String,
    /// API credentials
    pub credentials: PlatformCredentials,
    /// Default low stock threshold
    pub default_threshold: u32,
    /// Check interval in minutes
    pub check_interval_minutes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EcommercePlatform {
    Shopify,
    WooCommerce,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlatformCredentials {
    /// API key or access token
    pub api_key: String,
    /// API secret (for Shopify)
    pub api_secret: Option<String>,
    /// Password or consumer secret (for WooCommerce)
    pub password: Option<String>,
}

/// Product inventory information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProductInventory {
    pub id: String,
    pub sku: String,
    pub name: String,
    pub current_stock: i32,
    pub reserved_stock: i32,
    pub available_stock: i32,
    pub low_stock_threshold: u32,
    pub reorder_point: u32,
    pub reorder_quantity: u32,
    pub supplier_info: Option<SupplierInfo>,
    pub last_updated: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupplierInfo {
    pub name: String,
    pub contact_email: Option<String>,
    pub api_endpoint: Option<String>,
    pub lead_time_days: u32,
}

/// Low stock alert
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LowStockAlert {
    pub product: ProductInventory,
    pub severity: AlertSeverity,
    pub suggested_action: String,
    pub estimated_stockout_date: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertSeverity {
    Warning,  // Below threshold but not critical
    Critical, // Very low stock
    OutOfStock, // Zero stock
}

/// Reorder suggestion
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReorderSuggestion {
    pub product_id: String,
    pub product_name: String,
    pub sku: String,
    pub current_stock: i32,
    pub suggested_quantity: u32,
    pub unit_cost: Option<f64>,
    pub total_cost: Option<f64>,
    pub supplier: Option<SupplierInfo>,
    pub urgency: ReorderUrgency,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReorderUrgency {
    Low,      // Stock adequate for now
    Medium,   // Should reorder soon
    High,     // Reorder immediately
    Critical, // Emergency restock needed
}

/// Purchase order
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PurchaseOrder {
    pub id: String,
    pub supplier: SupplierInfo,
    pub items: Vec<OrderItem>,
    pub status: OrderStatus,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub expected_delivery: Option<chrono::DateTime<chrono::Utc>>,
    pub total_amount: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderItem {
    pub product_id: String,
    pub sku: String,
    pub product_name: String,
    pub quantity: u32,
    pub unit_price: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    Draft,
    PendingApproval,
    Submitted,
    Confirmed,
    Shipped,
    Delivered,
    Cancelled,
}

/// Inventory management tool
pub struct InventoryTool {
    config: InventoryConfig,
    http_client: reqwest::Client,
}

impl InventoryTool {
    /// Create new inventory tool
    pub fn new(config: InventoryConfig) -> anyhow::Result<Self> {
        let http_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        Ok(Self {
            config,
            http_client,
        })
    }

    /// Get product inventory
    pub async fn get_product_inventory(
        &self,
        product_id: &str,
    ) -> anyhow::Result<ProductInventory> {
        match self.config.platform {
            EcommercePlatform::Shopify => {
                self.get_shopify_product(product_id).await
            }
            EcommercePlatform::WooCommerce => {
                self.get_woocommerce_product(product_id).await
            }
        }
    }

    /// List all products with inventory
    pub async fn list_inventory(
        &self,
    ) -> anyhow::Result<Vec<ProductInventory>> {
        match self.config.platform {
            EcommercePlatform::Shopify => {
                self.list_shopify_inventory().await
            }
            EcommercePlatform::WooCommerce => {
                self.list_woocommerce_inventory().await
            }
        }
    }

    /// Check for low stock items
    pub async fn check_low_stock(
        &self,
    ) -> anyhow::Result<Vec<LowStockAlert>> {
        let products = self.list_inventory().await?;
        let mut alerts = Vec::new();

        for product in products {
            if product.available_stock <= 0 {
                alerts.push(LowStockAlert {
                    severity: AlertSeverity::OutOfStock,
                    suggested_action: format!(
                        "Emergency reorder {} immediately. Current stock: 0",
                        product.name
                    ),
                    product,
                    estimated_stockout_date: Some(chrono::Utc::now()),
                });
            } else if (product.available_stock as u32) < product.low_stock_threshold {
                let severity = if (product.available_stock as u32) < product.reorder_point {
                    AlertSeverity::Critical
                } else {
                    AlertSeverity::Warning
                };

                let days_until_stockout = if product.available_stock > 0 {
                    Some(chrono::Utc::now() + chrono::Duration::days(7)) // Estimate
                } else {
                    None
                };

                alerts.push(LowStockAlert {
                    severity,
                    suggested_action: format!(
                        "Reorder {}. Current: {}, Threshold: {}",
                        product.name, product.available_stock, product.low_stock_threshold
                    ),
                    product,
                    estimated_stockout_date: days_until_stockout,
                });
            }
        }

        // Sort by severity
        alerts.sort_by(|a, b| {
            let severity_order = |s: &AlertSeverity| match s {
                AlertSeverity::OutOfStock => 0,
                AlertSeverity::Critical => 1,
                AlertSeverity::Warning => 2,
            };
            severity_order(&a.severity).cmp(&severity_order(&b.severity))
        });

        Ok(alerts)
    }

    /// Generate reorder suggestions
    pub async fn suggest_reorders(
        &self,
    ) -> anyhow::Result<Vec<ReorderSuggestion>> {
        let alerts = self.check_low_stock().await?;
        let mut suggestions = Vec::new();

        for alert in alerts {
            let product = alert.product;
            let urgency = match alert.severity {
                AlertSeverity::OutOfStock => ReorderUrgency::Critical,
                AlertSeverity::Critical => ReorderUrgency::High,
                AlertSeverity::Warning => ReorderUrgency::Medium,
            };

            suggestions.push(ReorderSuggestion {
                product_id: product.id.clone(),
                product_name: product.name.clone(),
                sku: product.sku.clone(),
                current_stock: product.available_stock,
                suggested_quantity: product.reorder_quantity,
                unit_cost: None, // Would fetch from supplier API
                total_cost: None,
                supplier: product.supplier_info.clone(),
                urgency,
                reason: alert.suggested_action,
            });
        }

        Ok(suggestions)
    }

    /// Create purchase order (requires approval)
    pub async fn create_purchase_order(
        &self,
        supplier: &SupplierInfo,
        items: Vec<OrderItem>,
    ) -> anyhow::Result<PurchaseOrder> {
        let order = PurchaseOrder {
            id: format!("PO-{}", uuid::Uuid::new_v4().to_string()[..8].to_uppercase()),
            supplier: supplier.clone(),
            items,
            status: OrderStatus::Draft,
            created_at: chrono::Utc::now(),
            expected_delivery: Some(
                chrono::Utc::now() + chrono::Duration::days(supplier.lead_time_days as i64)
            ),
            total_amount: None, // Calculate from items
        };

        Ok(order)
    }

    /// Update stock levels (after receiving shipment)
    pub async fn update_stock(
        &self,
        product_id: &str,
        quantity_adjustment: i32,
        reason: &str,
    ) -> anyhow::Result<ProductInventory> {
        // In real implementation, would call platform API
        // For now, return mock updated inventory
        let mut product = self.get_product_inventory(product_id).await?;
        product.current_stock += quantity_adjustment;
        product.available_stock = product.current_stock - product.reserved_stock;
        product.last_updated = chrono::Utc::now();

        Ok(product)
    }

    // Shopify API methods
    async fn get_shopify_product(
        &self,
        product_id: &str,
    ) -> anyhow::Result<ProductInventory> {
        let url = format!(
            "{}/admin/api/2024-01/products/{}.json",
            self.config.store_url, product_id
        );

        let response = self.http_client
            .get(&url)
            .header("X-Shopify-Access-Token", &self.config.credentials.api_key)
            .send()
            .await?;

        if !response.status().is_success() {
            anyhow::bail!("Shopify API error: {}", response.status());
        }

        // Parse Shopify response (simplified)
        // In production, would parse actual Shopify product JSON
        Ok(ProductInventory {
            id: product_id.to_string(),
            sku: format!("SKU-{}", product_id),
            name: "Product Name".to_string(),
            current_stock: 10,
            reserved_stock: 2,
            available_stock: 8,
            low_stock_threshold: 5,
            reorder_point: 3,
            reorder_quantity: 20,
            supplier_info: None,
            last_updated: chrono::Utc::now(),
        })
    }

    async fn list_shopify_inventory(
        &self,
    ) -> anyhow::Result<Vec<ProductInventory>> {
        let url = format!(
            "{}/admin/api/2024-01/products.json?limit=250",
            self.config.store_url
        );

        let response = self.http_client
            .get(&url)
            .header("X-Shopify-Access-Token", &self.config.credentials.api_key)
            .send()
            .await?;

        if !response.status().is_success() {
            anyhow::bail!("Shopify API error: {}", response.status());
        }

        // Parse Shopify products (simplified mock)
        Ok(vec![
            ProductInventory {
                id: "prod_001".to_string(),
                sku: "TSHIRT-001".to_string(),
                name: "Premium T-Shirt".to_string(),
                current_stock: 15,
                reserved_stock: 3,
                available_stock: 12,
                low_stock_threshold: 10,
                reorder_point: 5,
                reorder_quantity: 50,
                supplier_info: Some(SupplierInfo {
                    name: "Textile Corp".to_string(),
                    contact_email: Some("orders@textilecorp.com".to_string()),
                    api_endpoint: None,
                    lead_time_days: 7,
                }),
                last_updated: chrono::Utc::now(),
            },
            ProductInventory {
                id: "prod_002".to_string(),
                sku: "MUG-002".to_string(),
                name: "Ceramic Mug".to_string(),
                current_stock: 3,
                reserved_stock: 1,
                available_stock: 2,
                low_stock_threshold: 10,
                reorder_point: 5,
                reorder_quantity: 30,
                supplier_info: Some(SupplierInfo {
                    name: "Ceramics Plus".to_string(),
                    contact_email: Some("sales@ceramicsplus.com".to_string()),
                    api_endpoint: None,
                    lead_time_days: 14,
                }),
                last_updated: chrono::Utc::now(),
            },
        ])
    }

    // WooCommerce API methods
    async fn get_woocommerce_product(
        &self,
        product_id: &str,
    ) -> anyhow::Result<ProductInventory> {
        let url = format!(
            "{}/wp-json/wc/v3/products/{}",
            self.config.store_url, product_id
        );

        let response = self.http_client
            .get(&url)
            .basic_auth(
                &self.config.credentials.api_key,
                self.config.credentials.password.as_ref()
            )
            .send()
            .await?;

        if !response.status().is_success() {
            anyhow::bail!("WooCommerce API error: {}", response.status());
        }

        // Parse WooCommerce response (simplified)
        Ok(ProductInventory {
            id: product_id.to_string(),
            sku: format!("SKU-{}", product_id),
            name: "WooCommerce Product".to_string(),
            current_stock: 20,
            reserved_stock: 5,
            available_stock: 15,
            low_stock_threshold: 10,
            reorder_point: 5,
            reorder_quantity: 25,
            supplier_info: None,
            last_updated: chrono::Utc::now(),
        })
    }

    async fn list_woocommerce_inventory(
        &self,
    ) -> anyhow::Result<Vec<ProductInventory>> {
        let url = format!(
            "{}/wp-json/wc/v3/products?per_page=100",
            self.config.store_url
        );

        let response = self.http_client
            .get(&url)
            .basic_auth(
                &self.config.credentials.api_key,
                self.config.credentials.password.as_ref()
            )
            .send()
            .await?;

        if !response.status().is_success() {
            anyhow::bail!("WooCommerce API error: {}", response.status());
        }

        // Parse WooCommerce products (simplified mock)
        Ok(vec![
            ProductInventory {
                id: "wc_001".to_string(),
                sku: "BOOK-001".to_string(),
                name: "Technical Manual".to_string(),
                current_stock: 8,
                reserved_stock: 2,
                available_stock: 6,
                low_stock_threshold: 10,
                reorder_point: 5,
                reorder_quantity: 20,
                supplier_info: Some(SupplierInfo {
                    name: "Publishers Inc".to_string(),
                    contact_email: Some("orders@publishers.com".to_string()),
                    api_endpoint: None,
                    lead_time_days: 10,
                }),
                last_updated: chrono::Utc::now(),
            },
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_inventory_tool_creation() {
        let config = InventoryConfig {
            platform: EcommercePlatform::Shopify,
            store_url: "https://test-store.myshopify.com".to_string(),
            credentials: PlatformCredentials {
                api_key: "test_key".to_string(),
                api_secret: None,
                password: None,
            },
            default_threshold: 10,
            check_interval_minutes: 60,
        };

        let tool = InventoryTool::new(config);
        assert!(tool.is_ok());
    }
}
