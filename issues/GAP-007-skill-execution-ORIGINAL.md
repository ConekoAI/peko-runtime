# GAP-007: Skill System Execution Engine

**Priority:** 🟠 High  
**Status:** Open  
**Target:** v0.6.0  
**Est. Effort:** 1 week  

---

## Problem Statement

The Grand Architecture specifies Skills as multi-step workflows built on tools/MCPs. Currently:
- `SkillsRegistry` loads skill manifests from TOML
- Skills define `SkillTool` structures
- **No execution engine** - skills are inert

Skills like `group_chat_manager`, `broadcast_hub` don't exist.

---

## Current State

```rust
// src/skills/mod.rs
pub struct Skill {
    pub name: String,
    pub description: String,
    pub tools: Vec<SkillTool>, // Defined but not executable
    pub prompts: Vec<String>,
}

pub struct SkillTool {
    pub name: String,
    pub description: String,
    pub kind: String,    // "shell", "http", "script"
    pub command: String, // Just strings, no execution!
    pub args: HashMap<String, String>,
}
```

Skills are loaded but cannot be executed.

---

## Target State

Per [GRAND_ARCHITECTURE.md section 4.5.3](../GRAND_ARCHITECTURE.md#453-skills-workflows):

```rust
// Skills are workflows that can be invoked
let result = skill_engine.execute(
    "group_chat_manager",
    json!({
        "room": "engineering",
        "action": "broadcast",
        "message": "Deploying to production"
    })
).await?;
```

---

## Scope

### In Scope
- Skill execution engine
- Skill tool runners (shell, http, script)
- Built-in skills: `group_chat_manager`, `broadcast_hub`
- Skill-to-tool binding
- Skill context sharing

### Out of Scope (Future)
- Visual workflow editor
- Skill marketplace (Pekohub integration)
- Skill versioning/upgrades

---

## Goals

1. **Execution Engine**: Run skills with input parameters
2. **Tool Runners**: Execute shell commands, HTTP requests, scripts
3. **Built-in Skills**: Implement core coordination skills
4. **Agent Integration**: Expose skills as invocable tools
5. **Context Access**: Skills can access agent context

---

## Proposed Implementation

### Skill Execution Engine
```rust
// src/skills/engine.rs
pub struct SkillEngine {
    registry: Arc<SkillsRegistry>,
    tool_registry: Arc<dyn ToolRegistry>,
    agent_context: Option<AgentContext>,
}

pub struct SkillExecution {
    pub skill: Skill,
    pub context: SkillContext,
    pub state: ExecutionState,
}

pub enum ExecutionState {
    Pending,
    Running,
    Completed(Value),
    Failed(String),
}

impl SkillEngine {
    pub async fn execute(
        &self,
        skill_name: &str,
        input: Value,
    ) -> Result<SkillResult> {
        let skill = self.registry.get(skill_name)
            .ok_or("Skill not found")?;

        let mut execution = SkillExecution::new(skill, input);

        for step in &skill.workflow_steps {
            self.execute_step(&mut execution, step).await?;
        }

        Ok(execution.into_result())
    }

    async fn execute_step(
        &self,
        execution: &mut SkillExecution,
        step: &WorkflowStep,
    ) -> Result<()> {
        match step {
            WorkflowStep::Tool { name, args } => {
                let tool = self.tool_registry.get(name)?;
                let result = tool.execute(args).await?;
                execution.context.set_result(result);
            }
            WorkflowStep::AgentSend { target, message } => {
                // Use agent_send tool
            }
            WorkflowStep::Condition { expr, then, else_ } => {
                // Evaluate and branch
            }
            WorkflowStep::Loop { ... } => { ... }
        }
        Ok(())
    }
}
```

### Skill Tool Runners
```rust
// src/skills/runners.rs
#[async_trait]
pub trait SkillToolRunner: Send + Sync {
    async fn run(&self, tool: &SkillTool, ctx: &SkillContext) -> Result<Value>;
}

pub struct ShellRunner;
impl SkillToolRunner for ShellRunner {
    async fn run(&self, tool: &SkillTool, ctx: &SkillContext) -> Result<Value> {
        let output = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&tool.command)
            .envs(&tool.args)
            .output()
            .await?;

        Ok(json!({
            "stdout": String::from_utf8_lossy(&output.stdout),
            "stderr": String::from_utf8_lossy(&output.stderr),
            "exit_code": output.status.code(),
        }))
    }
}

pub struct HttpRunner;
impl SkillToolRunner for HttpRunner {
    async fn run(&self, tool: &SkillTool, ctx: &SkillContext) -> Result<Value> {
        let client = reqwest::Client::new();
        let response = client
            .request(method, &tool.command)
            .send()
            .await?;

        Ok(json!({
            "status": response.status().as_u16(),
            "body": response.text().await?,
        }))
    }
}
```

### Built-in Skills

#### group_chat_manager
```toml
# skills/group_chat_manager/SKILL.toml
[skill]
name = "group_chat_manager"
description = "Manage multi-agent group conversations"
version = "1.0.0"

[[tools]]
name = "broadcast"
description = "Send message to all participants"
kind = "builtin"
command = "broadcast"

[[tools]]
name = "add_participant"
description = "Add agent to room"
kind = "builtin"
command = "add_participant"
```

```rust
// src/skills/builtin/group_chat.rs
pub struct GroupChatManager {
    rooms: HashMap<String, Room>,
}

pub struct Room {
    participants: Vec<String>,
    history: Vec<Message>,
}

impl GroupChatManager {
    pub async fn broadcast(&self, room: &str, message: &str) -> Result<()> {
        for participant in &self.rooms[room].participants {
            agent_send(participant, message).await?;
        }
        Ok(())
    }
}
```

#### broadcast_hub
```rust
// src/skills/builtin/broadcast.rs
pub struct BroadcastHub {
    channels: Vec<String>,
}

impl BroadcastHub {
    pub async fn broadcast(&self, message: &str) -> Result<()> {
        for channel in &self.channels {
            agent_send(channel, message).await?;
        }
        Ok(())
    }
}
```

### Workflow DSL Extension
```rust
// Extended skill manifest with workflow
pub struct WorkflowStep {
    pub id: String,
    pub action: StepAction,
    pub on_error: ErrorAction,
}

pub enum StepAction {
    Tool { name: String, args: Value },
    AgentSend { target_expr: String, message_expr: String },
    Condition { expr: String, then: Vec<WorkflowStep>, else_: Vec<WorkflowStep> },
    Parallel { branches: Vec<Vec<WorkflowStep>> },
    Wait { duration_secs: u64 },
}
```

---

## Dependencies

- **Requires:** GAP-002 (Async execution for parallel steps)
- **Requires:** GAP-005 (Agent messaging for coordination skills)
- **Related to:** GAP-001 (MCP for external skill capabilities)

---

## Success Criteria

- [ ] Can execute a skill from TOML manifest
- [ ] Shell commands in skills execute and return output
- [ ] HTTP requests in skills work
- [ ] `group_chat_manager` skill can broadcast to agents
- [ ] `broadcast_hub` skill can pub-sub messages
- [ ] Skills can be invoked as tools by agents

---

## References

- [GRAND_ARCHITECTURE.md - Skills](../GRAND_ARCHITECTURE.md#453-skills-workflows)
- [GRAND_ARCHITECTURE.md - Complex Coordination](../GRAND_ARCHITECTURE.md#84-complex-coordination-external-skill)
- Current skills: `src/skills/mod.rs`
