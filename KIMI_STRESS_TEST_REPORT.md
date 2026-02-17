# Kimi API Stress Test Report

**Date:** February 17, 2026  
**Status:** ⏳ Clarification needed on API type

## Important Distinction

There are **two different** Kimi APIs:

### 1. Moonshot API (`KimiProvider`)
- **Endpoint:** `https://api.moonshot.cn`
- **Format:** OpenAI-compatible
- **Auth:** Bearer token
- **Pricing:** Pay-per-token
- **Use case:** General Kimi model access

### 2. Kimi Code (`KimiCodeProvider`) ✅ **THIS IS WHAT YOU NEED**
- **Endpoint:** `https://api.kimi-code.moonshot.cn` (or similar)
- **Format:** Anthropic-compatible (uses Claude Code backend)
- **Auth:** `x-api-key` header
- **Pricing:** Subscription-based
- **Use case:** Coding assistant

## The Problem

The 401 error occurred because we were trying to use **Moonshot API** format with what appears to be a **Kimi Code** key.

## Updated Implementation

I've added a **new provider** specifically for Kimi Code:

```rust
// src/providers/kimi_code.rs
pub struct KimiCodeProvider;
```

**Key differences from Moonshot API:**
- Uses `x-api-key` header (like Anthropic)
- Uses `/v1/messages` endpoint (like Anthropic)
- Strips `kimi-` prefix from keys if present
- Reads `KIMI_API_KEY`, `KIMICODE_API_KEY`, or `MOONSHOT_API_KEY`

## How to Test Kimi Code

### Option 1: Environment Variable
```bash
export KIMI_API_KEY="your-kimi-code-api-key"
# Or if your key has the kimi- prefix:
export KIMI_API_KEY="kimi-your-actual-key"
```

### Option 2: Using the Provider Directly
```rust
use pekobot::providers::{KimiCodeProvider, Provider};

let provider = KimiCodeProvider::from_env()?;
let response = provider.complete("Hello!").await?;
```

## Testing Both Providers

### Test Moonshot API (if you have that key)
```bash
export MOONSHOT_API_KEY="sk-..."
cargo run --example kimi_api_test
```

### Test Kimi Code (subscription-based)
```bash
export KIMI_API_KEY="your-key"
# Run a test with KimiCodeProvider
```

## Manual Verification

### For Moonshot API:
```bash
curl https://api.moonshot.cn/v1/chat/completions \
  -H "Authorization: Bearer YOUR_KEY" \
  -H "Content-Type: application/json" \
  -d '{"model": "kimi-k2.5", "messages": [{"role": "user", "content": "Hello"}]}'
```

### For Kimi Code:
```bash
curl https://api.kimi-code.moonshot.cn/v1/messages \
  -H "x-api-key: YOUR_KEY" \
  -H "anthropic-version: 2023-06-01" \
  -H "Content-Type: application/json" \
  -d '{
    "model": "kimi-k2.5",
    "max_tokens": 1024,
    "messages": [{"role": "user", "content": "Hello"}]
  }'
```

## Pekobot Provider Stats

- **Total Providers:** 15 (added Kimi Code)
- **Kimi (Moonshot):** `src/providers/kimi.rs` - OpenAI-compatible
- **Kimi Code:** `src/providers/kimi_code.rs` - Anthropic-compatible ⭐ NEW

## Next Steps

1. Confirm which API key you have (Moonshot or Kimi Code)
2. If Kimi Code: Use the new `KimiCodeProvider`
3. If Moonshot: The existing `KimiProvider` should work
4. Run appropriate test based on your key type

---

*Updated by Pekora (Pekobot) 🐰*  
*Now supporting both Moonshot API and Kimi Code!*
