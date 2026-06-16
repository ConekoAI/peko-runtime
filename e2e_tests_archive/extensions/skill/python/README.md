# Skill Capability E2E Test

This E2E test demonstrates how Pekobot's skill system works end-to-end, from installation to agent usage.

## Overview

The test verifies:
1. **Skill Installation** - Installing a skill from a local directory
2. **Skill Discovery** - Listing and showing skill information
3. **Skill Enablement** - Enabling skills for agents via CLI
4. **Skill Usage** - Agent using skills in conversations
5. **Session Verification** - Skills appearing in session history

## Files

| File | Purpose |
|------|---------|
| `test.ps1` | Main E2E test script |
| `calculator-skill/SKILL.md` | Example skill following Anthropic spec |
| `README.md` | This file |

## Quick Start

```powershell
# Set your API key
$env:MINIMAX_API_KEY = "your-api-key"

# Run the E2E test
.\e2e_tests\cap\skill\python\test.ps1
```

## Skill Format

The test uses a simple calculator skill that follows the [Anthropic Skills specification](https://github.com/anthropics/skills):

```markdown
---
name: calculator-skill
description: Perform arithmetic calculations with clear step-by-step explanations
tags: [math, calculator, arithmetic]
author: Pekobot E2E Test
---

# Calculator Skill

## Instructions
When the user asks for calculations:
1. Identify the arithmetic operation needed
2. Perform the calculation accurately
3. Provide the result with a clear explanation
...
```

## CLI Commands Tested

```bash
# Install skill
pekobot cap skill install ./calculator-skill

# List skills
pekobot cap skill list
pekobot cap skill list --long

# Show skill info
pekobot cap skill info calculator-skill

# Read skill content
pekobot cap skill read calculator-skill

# Enable for agent
pekobot cap enable default/my-agent calculator-skill

# Check status
pekobot cap status default/my-agent

# Uninstall skill
pekobot cap skill uninstall calculator-skill --force
```

## What the Test Does

### 1. Install Skill
Copies the calculator-skill directory to `{data_dir}/skills/calculator-skill/`

### 2. Verify Installation
- Lists installed skills
- Shows skill info (name, description, tags, author)
- Reads full SKILL.md content

### 3. Create Agent & Enable Skill
- Creates a test agent
- Enables the calculator-skill for the agent
- Updates agent config's `tools.skills` array

### 4. Test Skill Usage
- Sends a message to the agent requesting calculation
- Agent should reference having the calculator skill
- Should provide formatted calculation response

### 5. Verify Session
- Checks session history for skill reference
- Verifies `available_skills` appears in system prompt

### 6. Cleanup
- Deletes test agent
- Uninstalls test skill

## Expected Output

```
✓ Skill 'calculator-skill' installed successfully
✓ Skill info shows correct details
✓ Agent created
✓ Skill enabled for agent
✓ Skill appears in agent status
✓ Agent response contains calculation result
✓ Session created
✓ Skill 'calculator-skill' found in session
✓ Skills section or calculator reference found
✓ Skill read command works correctly
✓ Skill appears in 'cap list --type skill'
✅ Skill Capability E2E test completed!
```

## Comparison with Other Capability Types

| Feature | Universal Tools | MCP | Skills |
|---------|----------------|-----|--------|
| Install | `cap universal install` | `cap mcp add` | `cap skill install` |
| Enable | `cap enable agent tool` | `cap enable agent tool` | `cap enable agent skill` |
| Type | Executable | Server process | Documentation |
| Runtime | Subprocess | JSON-RPC stdio/SSE | Injected into prompt |
| Reserved params | Supported | Supported | N/A |

## See Also

- [MCP E2E Test](../mcp/python/) - Compare with MCP workflow
- [Universal Tool E2E Test](../tool/custom/python/) - Compare with Universal Tools
