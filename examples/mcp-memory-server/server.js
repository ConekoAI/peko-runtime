#!/usr/bin/env node
/**
 * MCP Memory Server with Reserved Parameter Injection Demo
 * 
 * This server demonstrates how MCP tools can receive reserved parameters
 * that are injected by the Peko runtime (hidden from the LLM).
 * 
 * The server stores/retrieves key-value pairs that are automatically
 * isolated per agent_id.
 */

const { Server } = require("@modelcontextprotocol/sdk/server/index.js");
const { StdioServerTransport } = require("@modelcontextprotocol/sdk/server/stdio.js");
const {
  CallToolRequestSchema,
  ListToolsRequestSchema,
} = require("@modelcontextprotocol/sdk/types.js");

// In-memory storage: { "agent_id:key": value }
const storage = new Map();

// Create MCP server
const server = new Server(
  {
    name: "peko-memory-server",
    version: "1.0.0",
  },
  {
    capabilities: {
      tools: {},
    },
  }
);

/**
 * List available tools
 * 
 * Note: agent_id and session_id are marked as NOT required.
 * They will be injected by Peko if reserved_parameters are configured.
 */
server.setRequestHandler(ListToolsRequestSchema, async () => {
  return {
    tools: [
      {
        name: "memory_store",
        description: "Store a value in memory. Keys are automatically isolated per agent.",
        inputSchema: {
          type: "object",
          properties: {
            key: {
              type: "string",
              description: "Memory key (e.g., 'user_name', 'preferences')"
            },
            value: {
              type: "string",
              description: "Value to store"
            },
            // These are injected by Peko - NOT visible to LLM
            agent_id: {
              type: "string",
              description: "Agent identifier (auto-injected)"
            },
            session_id: {
              type: "string",
              description: "Session identifier (auto-injected)"
            }
          },
          required: ["key", "value"]
          // Note: agent_id and session_id are NOT required
          // They'll be injected if configured, or null if not
        }
      },
      {
        name: "memory_retrieve",
        description: "Retrieve a value from memory. Automatically uses the agent's isolated namespace.",
        inputSchema: {
          type: "object",
          properties: {
            key: {
              type: "string",
              description: "Memory key to retrieve"
            },
            // These are injected by Peko
            agent_id: {
              type: "string",
              description: "Agent identifier (auto-injected)"
            },
            session_id: {
              type: "string",
              description: "Session identifier (auto-injected)"
            }
          },
          required: ["key"]
        }
      },
      {
        name: "memory_list",
        description: "List all memory keys for the current agent",
        inputSchema: {
          type: "object",
          properties: {
            // These are injected by Peko
            agent_id: {
              type: "string",
              description: "Agent identifier (auto-injected)"
            },
            session_id: {
              type: "string",
              description: "Session identifier (auto-injected)"
            }
          },
          required: []
        }
      },
      {
        name: "memory_delete",
        description: "Delete a memory key for the current agent",
        inputSchema: {
          type: "object",
          properties: {
            key: {
              type: "string",
              description: "Memory key to delete"
            },
            // These are injected by Peko
            agent_id: {
              type: "string",
              description: "Agent identifier (auto-injected)"
            },
            session_id: {
              type: "string",
              description: "Session identifier (auto-injected)"
            }
          },
          required: ["key"]
        }
      }
    ]
  };
});

/**
 * Helper to build isolated storage key
 */
function buildKey(agentId, userKey) {
  // Default to 'anonymous' if no agent_id injected
  const prefix = agentId || 'anonymous';
  return `${prefix}:${userKey}`;
}

/**
 * Handle tool calls
 */
server.setRequestHandler(CallToolRequestSchema, async (request) => {
  const { name, arguments: args } = request.params;
  
  // Extract injected parameters (if any)
  const agentId = args.agent_id || null;
  const sessionId = args.session_id || null;
  
  console.error(`[Memory Server] Tool call: ${name}`);
  console.error(`[Memory Server] Agent ID: ${agentId || 'not injected'}`);
  console.error(`[Memory Server] Session ID: ${sessionId || 'not injected'}`);
  
  switch (name) {
    case "memory_store": {
      const { key, value } = args;
      const storageKey = buildKey(agentId, key);
      
      storage.set(storageKey, {
        value,
        stored_at: new Date().toISOString(),
        agent_id: agentId,
        session_id: sessionId
      });
      
      return {
        content: [
          {
            type: "text",
            text: JSON.stringify({
              success: true,
              key: key,
              isolated_key: storageKey,
              agent_id: agentId,
              message: `Stored '${key}' for agent '${agentId || 'anonymous'}'`
            }, null, 2)
          }
        ]
      };
    }
    
    case "memory_retrieve": {
      const { key } = args;
      const storageKey = buildKey(agentId, key);
      const entry = storage.get(storageKey);
      
      if (!entry) {
        return {
          content: [
            {
              type: "text",
              text: JSON.stringify({
                success: false,
                key: key,
                error: `Key '${key}' not found for agent '${agentId || 'anonymous'}'`
              }, null, 2)
            }
          ],
          isError: true
        };
      }
      
      return {
        content: [
          {
            type: "text",
            text: JSON.stringify({
              success: true,
              key: key,
              value: entry.value,
              stored_at: entry.stored_at,
              agent_id: agentId
            }, null, 2)
          }
        ]
      };
    }
    
    case "memory_list": {
      const prefix = buildKey(agentId, '');
      const keys = [];
      
      for (const [storageKey, entry] of storage.entries()) {
        if (storageKey.startsWith(prefix)) {
          const userKey = storageKey.slice(prefix.length);
          keys.push({
            key: userKey,
            stored_at: entry.stored_at
          });
        }
      }
      
      return {
        content: [
          {
            type: "text",
            text: JSON.stringify({
              success: true,
              agent_id: agentId,
              count: keys.length,
              keys: keys
            }, null, 2)
          }
        ]
      };
    }
    
    case "memory_delete": {
      const { key } = args;
      const storageKey = buildKey(agentId, key);
      const existed = storage.has(storageKey);
      storage.delete(storageKey);
      
      return {
        content: [
          {
            type: "text",
            text: JSON.stringify({
              success: true,
              key: key,
              existed,
              agent_id: agentId,
              message: existed 
                ? `Deleted '${key}' for agent '${agentId || 'anonymous'}'`
                : `Key '${key}' did not exist for agent '${agentId || 'anonymous'}'`
            }, null, 2)
          }
        ]
      };
    }
    
    default:
      return {
        content: [
          {
            type: "text",
            text: `Unknown tool: ${name}`
          }
        ],
        isError: true
      };
  }
});

// Start server
async function main() {
  const transport = new StdioServerTransport();
  await server.connect(transport);
  console.error("[Memory Server] Running on stdio");
}

main().catch(console.error);
