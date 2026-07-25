#!/usr/bin/env node
/**
 * Identity Echo Tool - E2E Test for Context Injection Verification (Node.js)
 * 
 * This tool explicitly reports back the injected identity parameters
 * to verify that context injection is working correctly.
 */

const { tool, run } = require('./pekobot_adapter');

const identityTool = tool(
    {
        name: "echo_identity",
        description: "Echo back the injected identity parameters to verify context injection",
        llm_description: `
Use this tool to verify that identity parameters (agent_id, session_id) are being injected.
The tool will return the injected values, which should be:
- agent_id: The name of the calling agent
- session_id: The current session ID
- run_id: The current run ID

Call this when asked to verify context injection is working.
`,
        parameters: {
            message: {
                type: "string",
                description: "Optional message to echo back"
            }
        },
        reserved: ["session_id", "agent_id", "run_id", "workspace"]
    },
    async ({ message, session_id, agent_id, run_id, workspace }) => {
        /**
         * Echo back identity parameters to verify context injection.
         * 
         * @param {string} message - Optional message from LLM
         * @param {string} session_id - Injected by Pekobot
         * @param {string} agent_id - Injected by Pekobot
         * @param {string} run_id - Injected by Pekobot
         * @param {string} workspace - Injected by Pekobot
         */
        
        // Check if parameters were actually injected
        const injectionWorking = (
            agent_id && agent_id !== "not_injected" &&
            session_id && session_id !== "not_injected"
        );
        
        return {
            success: true,
            message: message || "Context injection test",
            injected_identity: {
                agent_id: agent_id || "NOT_INJECTED",
                session_id: session_id || "NOT_INJECTED",
                run_id: run_id || "NOT_INJECTED",
                workspace: workspace || "NOT_INJECTED"
            },
            injection_working: injectionWorking,
            verification: "Context injection is " + (injectionWorking ? "WORKING" : "NOT WORKING")
        };
    }
);

run(identityTool);
