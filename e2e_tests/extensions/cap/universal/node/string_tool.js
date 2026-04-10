#!/usr/bin/env node
/**
 * String Tool - E2E Test Universal Tool for Pekobot (Node.js)
 * 
 * Demonstrates:
 * 1. Basic tool functionality
 * 2. Reserved parameter injection (session_id, agent_id)
 * 3. Returning structured results
 */

const { tool, run } = require('./pekobot_adapter');

const stringTool = tool(
    {
        name: "string_utils",
        description: "String manipulation utilities",
        llm_description: `
Use when you need to manipulate or analyze text strings.

Examples:
- "Convert this to uppercase"
- "Count the words in this text"
- "Reverse this string"
- "Check if this contains 'hello'"

Operations: uppercase, lowercase, reverse, wordcount, contains
`,
        parameters: {
            operation: {
                type: "string",
                description: "Operation to perform: uppercase, lowercase, reverse, wordcount, contains",
                enum: ["uppercase", "lowercase", "reverse", "wordcount", "contains"]
            },
            text: {
                type: "string",
                description: "Input text to process"
            },
            substring: {
                type: "string",
                description: "Substring to search for (only for 'contains' operation)"
            }
        },
        reserved: ["session_id", "agent_id"]
    },
    async ({ operation, text, substring, session_id, agent_id }) => {
        /**
         * Process string operations.
         * 
         * @param {string} operation - The operation to perform
         * @param {string} text - Input text (from LLM)
         * @param {string} substring - For contains operation (from LLM)
         * @param {string} session_id - Injected by Pekobot
         * @param {string} agent_id - Injected by Pekobot
         */
        
        if (!text) {
            return { success: false, error: "Missing required parameter: text" };
        }

        let result;
        
        switch (operation) {
            case "uppercase":
                result = text.toUpperCase();
                break;
            case "lowercase":
                result = text.toLowerCase();
                break;
            case "reverse":
                result = text.split('').reverse().join('');
                break;
            case "wordcount":
                result = text.trim().split(/\s+/).filter(w => w.length > 0).length;
                break;
            case "contains":
                if (!substring) {
                    return { success: false, error: "Missing required parameter: substring (required for 'contains' operation)" };
                }
                result = text.includes(substring);
                break;
            default:
                return { success: false, error: `Unknown operation: ${operation}` };
        }

        return {
            success: true,
            data: {
                result,
                operation,
                input_length: text.length
            },
            metadata: {
                processed_by: agent_id,
                session: session_id ? session_id.substring(0, 8) + "..." : "none",
                tool_version: "1.0.0",
                runtime: "node"
            }
        };
    }
);

run(stringTool);
