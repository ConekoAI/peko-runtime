---
name: deploy
description: Production deployment workflow - test, build, and deploy applications
tags: [devops, ci-cd]
author: Pekobot
---

# Deploy Skill

Use this skill when deploying applications to production or staging environments.

## When to Use

✅ **Use this skill for:**
- Deploying to production
- Deploying to staging
- Rolling back failed deployments
- Running pre-deployment checks

❌ **Don't use for:**
- Local development → use local dev commands
- Database migrations → use migration-specific tools

## Workflow

### 1. Pre-deployment Checks

Always run these first:

```bash
# Run tests
cargo test --all-features

# Check formatting
cargo fmt --check

# Run clippy
cargo clippy -- -D warnings
```

### 2. Build

```bash
# Build release binary
cargo build --release
```

### 3. Deploy

```bash
# Copy binary to server
scp target/release/myapp user@server:/opt/myapp/

# Restart service
ssh user@server "sudo systemctl restart myapp"
```

### 4. Verify

```bash
# Check service status
ssh user@server "sudo systemctl status myapp"

# Test health endpoint
curl https://api.example.com/health
```

## Rollback

If something goes wrong:

```bash
# Quick rollback
ssh user@server "sudo systemctl stop myapp && sudo cp /opt/myapp/backup/myapp /opt/myapp/myapp && sudo systemctl start myapp"
```

## Safety Checklist

Before deploying:
- [ ] Tests pass
- [ ] Code reviewed
- [ ] Database migrations prepared
- [ ] Rollback plan ready
- [ ] Monitoring alerts configured
