//! Secret Manager Example
//!
//! Demonstrates the complete Secret Manager workflow:
//! 1. Creating and unlocking the secret store
//! 2. Storing and retrieving secrets
//! 3. Managing permissions
//! 4. Using secrets in configuration
//! 5. Audit logging
//!
//! Run with:
//!   cargo run --example secret_manager_demo

use pekobot::secrets::{
    SecretManager, SecretPermission, SecretResolver, SecretScope, SecretType,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("🔐 Pekobot Secret Manager Demo\n");

    // Create a temporary store for demo
    let temp_dir = tempfile::tempdir()?;
    let store_path = temp_dir.path().join("demo-secrets.db");

    // =================================================================
    // Step 1: Create and Unlock Store
    // =================================================================
    println!("1️⃣ Creating and unlocking secret store...");

    let mut manager = SecretManager::open(&store_path)?;
    println!("   Store path: {:?}", store_path);
    println!("   Initial state: {}", if manager.is_unlocked() { "unlocked" } else { "locked" });

    // Unlock with master password
    manager.unlock("my-secure-master-password").await?;
    println!("   After unlock: {}", if manager.is_unlocked() { "unlocked ✅" } else { "locked ❌" });

    // =================================================================
    // Step 2: Store Secrets
    // =================================================================
    println!("\n2️⃣ Storing secrets...");

    // Store a global API key
    let entry = manager
        .set(
            "OPENAI_API_KEY",
            SecretScope::Global,
            "sk-demo123456789",
            SecretType::ApiKey,
            Some(pekobot::secrets::SecretMetadata {
                description: Some("OpenAI API Key for GPT-4 access".to_string()),
                source_hint: Some("https://platform.openai.com/api-keys".to_string()),
                expires_at: None,
                tags: vec!["production".to_string()],
            }),
        )
        .await?;

    println!("   Stored: {} (v{})", entry.name, entry.version);

    // Store a per-agent secret
    let shopify_agent = "did:pekobot:local:shopify-bot:abc123";
    let entry = manager
        .set(
            "SHOPIFY_TOKEN",
            SecretScope::Agent {
                did: shopify_agent.to_string(),
            },
            "shpat_demo_xxxxx",
            SecretType::Token,
            None,
        )
        .await?;

    println!("   Stored: {} (scoped to agent)", entry.name);

    // =================================================================
    // Step 3: Retrieve Secrets
    // =================================================================
    println!("\n3️⃣ Retrieving secrets...");

    let api_key = manager
        .get("OPENAI_API_KEY", &SecretScope::Global)
        .await?;
    println!("   OPENAI_API_KEY: {}", api_key.unwrap_or_else(|| "not found".to_string()));

    // =================================================================
    // Step 4: List All Secrets
    // =================================================================
    println!("\n4️⃣ Listing secrets...");

    let secrets = manager.list(None).await?;
    println!("   Total secrets: {}", secrets.len());
    for secret in &secrets {
        let scope = match &secret.scope {
            SecretScope::Global => "global".to_string(),
            SecretScope::Agent { did } => format!("agent:{:.16}...", did),
        };
        println!("   - {} ({:?}, {})", secret.name, secret.secret_type, scope);
    }

    // =================================================================
    // Step 5: Permission Management
    // =================================================================
    println!("\n5️⃣ Managing permissions...");

    // Check default permission (global secrets default to Read)
    let perm = manager
        .check_permission("OPENAI_API_KEY", &SecretScope::Global, None)
        .await?;
    println!("   Default permission for global secrets: {:?}", perm);

    // Deny access to a specific agent
    let untrusted_agent = "did:pekobot:local:untrusted:xyz789";
    manager
        .grant_permission(
            "OPENAI_API_KEY",
            &SecretScope::Global,
            Some(untrusted_agent),
            SecretPermission::None,
        )
        .await?;
    println!("   Denied access to: {:.16}...", untrusted_agent);

    // Grant read access to another agent
    let trusted_agent = "did:pekobot:local:trusted:def456";
    manager
        .grant_permission(
            "OPENAI_API_KEY",
            &SecretScope::Global,
            Some(trusted_agent),
            SecretPermission::Read,
        )
        .await?;
    println!("   Granted Read access to: {:.16}...", trusted_agent);

    // List permissions
    let perms = manager
        .get_permissions("OPENAI_API_KEY", &SecretScope::Global)
        .await?;
    println!("   Explicit permissions set: {}", perms.len());

    // =================================================================
    // Step 6: Secret Resolution in Config
    // =================================================================
    println!("\n6️⃣ Using secrets in configuration...");

    // Lock and use resolver
    manager.lock();

    let resolver = SecretResolver::open(&store_path)?;
    resolver.unlock("my-secure-master-password").await?;

    // Resolve ${secret:NAME} syntax
    let config_value = "Authorization: Bearer ${secret:OPENAI_API_KEY}";
    let resolved = resolver.resolve(config_value).await?;
    println!("   Input:  {}", config_value);
    println!("   Output: {}", resolved);

    // Resolve agent-scoped secret
    let agent_config = format!("shopify_token = \"${{secret.agent:{}:SHOPIFY_TOKEN}}\"", shopify_agent);
    let resolved = resolver.resolve(&agent_config).await?;
    println!("   Input:  ...shopify_token = \"${{secret.agent:DID:SHOPIFY_TOKEN}}\"");
    println!("   Output: {}", resolved);

    // =================================================================
    // Step 7: Audit Log
    // =================================================================
    println!("\n7️⃣ Checking audit log...");

    // Re-unlock manager for audit queries
    manager.unlock("my-secure-master-password").await?;

    let stats = manager.get_audit_stats(None).await?;
    println!("   Audit stats:");
    println!("     Total events:   {}", stats.total);
    println!("     Successful:     {} ({:.1}%)", stats.successful, stats.success_rate());
    println!("     Failed:         {}", stats.failed);

    let recent_events = manager
        .query_audit_log(None, None, None, None, 5)
        .await?;
    println!("   Recent events (last 5):");
    for event in &recent_events {
        println!(
            "     [{}] {:?} - {} ({})",
            &event.timestamp[..19],
            event.event,
            event.secret_name,
            if event.success { "✓" } else { "✗" }
        );
    }

    // =================================================================
    // Step 8: Cleanup
    // =================================================================
    println!("\n8️⃣ Cleanup...");

    // Delete demo secrets
    manager
        .delete("OPENAI_API_KEY", &SecretScope::Global)
        .await?;
    println!("   Deleted OPENAI_API_KEY");

    manager
        .delete(
            "SHOPIFY_TOKEN",
            &SecretScope::Agent {
                did: shopify_agent.to_string(),
            },
        )
        .await?;
    println!("   Deleted SHOPIFY_TOKEN");

    // Lock store
    manager.lock();
    println!("   Store locked");

    println!("\n✅ Secret Manager demo complete!");
    println!("\nTry these commands:");
    println!("  pekobot secret set OPENAI_API_KEY --type api_key");
    println!("  pekobot secret get OPENAI_API_KEY");
    println!("  pekobot secret list");
    println!("  pekobot secret audit --stats");

    Ok(())
}
