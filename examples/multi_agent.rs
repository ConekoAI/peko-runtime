//! Multi-Agent Orchestration Example
//!
//! Demonstrates how to set up multiple agents that can communicate
//! using the A2A protocol and work together on tasks.

use pekobot::{agent::Orchestrator, a2a::registry::create_registry, Agent, Config};
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    println!("🐰 Multi-Agent Orchestration Example\n");
    println!("=====================================\n");

    // Create a shared registry for agent communication
    let (registry, receiver) = create_registry();
    let mut orchestrator = Orchestrator::with_registry(registry);

    // Create specialized agents
    println!("🏗️  Creating agents...\n");

    // Agent 1: Task Planner
    let planner_config = Config::agent("task-planner")
        .with_description("Plans and breaks down complex tasks")
        .with_capabilities(vec!["planning".to_string(), "coordination".to_string()])
        .with_memory(true)
        .build();

    let planner = Agent::new(planner_config).await?;
    println!("   ✅ Task Planner: {}", planner.did());
    orchestrator.add_agent(planner).await?;

    // Agent 2: Research Agent
    let research_config = Config::agent("research-agent")
        .with_description("Gathers and analyzes information")
        .with_capabilities(vec!["research".to_string(), "analysis".to_string()])
        .with_memory(true)
        .build();

    let researcher = Agent::new(research_config).await?;
    println!("   ✅ Research Agent: {}", researcher.did());
    orchestrator.add_agent(researcher).await?;

    // Agent 3: Execution Agent
    let exec_config = Config::agent("execution-agent")
        .with_description("Executes tasks and reports results")
        .with_capabilities(vec!["execution".to_string(), "reporting".to_string()])
        .with_memory(true)
        .build();

    let executor = Agent::new(exec_config).await?;
    println!("   ✅ Execution Agent: {}\n", executor.did());
    orchestrator.add_agent(executor).await?;

    // Start all agents
    println!("🚀 Starting all agents...\n");
    orchestrator.start_all().await?;

    // List all registered agents
    let agents = orchestrator.list_agents().await;
    println!("📋 Registered Agents ({}):", agents.len());
    for (did, name) in &agents {
        println!("   • {} ({})", name, &did[..20.min(did.len())]);
    }
    println!();

    // Demonstrate agent lookup
    println!("🔍 Finding agent by name 'research-agent'...");
    if let Some(agent) = orchestrator.find_by_name("research-agent").await {
        println!("   Found!\n");
    }

    // Simulate a workflow
    println!("🎬 Simulating multi-agent workflow:\n");
    
    let workflow_steps = vec![
        ("task-planner", "Break down: How to make coffee"),
        ("research-agent", "What are the best coffee beans?"),
        ("execution-agent", "Execute: Grind beans and brew"),
    ];

    for (agent_name, task) in workflow_steps {
        if let Some(agent) = orchestrator.find_by_name(agent_name).await {
            let agent = agent.lock().await;
            println!("   [{}] Task: {}", agent_name, task);
            let result = agent.execute(task).await?;
            println!("   [{}] Result: {}\n", agent_name, result.trim());
        }
    }

    // Stop all agents
    println!("🛑 Stopping all agents...");
    orchestrator.stop_all().await?;
    println!("   ✅ All agents stopped\n");

    println!("👋 Multi-agent orchestration complete!");

    Ok(())
}
