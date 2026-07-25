#!/usr/bin/env node
/**
 * Pekobot Universal Tool Adapter for Node.js (E2E Test Version)
 * 
 * Minimal adapter for E2E testing - handles JSON-RPC protocol over stdio.
 */

const readline = require('readline');

/**
 * Create a tool definition
 * @param {Object} config - Tool configuration
 * @param {Function} handler - Tool handler function
 */
function tool(config, handler) {
    return {
        config: {
            name: config.name,
            description: config.description,
            parameters: config.parameters || {},
            reserved: config.reserved || [],
            llm_description: config.llm_description
        },
        handler
    };
}

/**
 * Run the protocol loop
 * @param {Object} toolDef - Tool definition from tool()
 */
function run(toolDef) {
    const schema = {
        type: "object",
        properties: {},
        required: []
    };

    // Build parameter schema
    for (const [name, spec] of Object.entries(toolDef.config.parameters)) {
        schema.properties[name] = spec;
        if (!spec.default !== undefined) {
            schema.required.push(name);
        }
    }

    const rl = readline.createInterface({
        input: process.stdin,
        output: process.stdout,
        terminal: false
    });

    rl.on('line', (line) => {
        const trimmed = line.trim();
        if (!trimmed) return;

        try {
            const request = JSON.parse(trimmed);
            const response = handleRequest(request, toolDef, schema);
            console.log(JSON.stringify(response));
        } catch (e) {
            console.log(JSON.stringify(errorResponse(null, -32700, `Parse error: ${e.message}`)));
        }
    });
}

/**
 * Handle incoming request
 */
function handleRequest(request, toolDef, schema) {
    const { method, id, params = {} } = request;

    if (method === 'tool/describe') {
        return handleDescribe(id, toolDef, schema);
    } else if (method === 'tool/execute') {
        return handleExecute(id, params, toolDef);
    } else {
        return errorResponse(id, -32601, `Method '${method}' not found`);
    }
}

/**
 * Handle tool/describe
 */
function handleDescribe(id, toolDef, schema) {
    const result = {
        name: toolDef.config.name,
        description: toolDef.config.description,
        parameters: schema
    };

    if (toolDef.config.llm_description) {
        result.llm_description = toolDef.config.llm_description;
    }

    if (toolDef.config.reserved.length > 0) {
        result.reserved_parameters = {};
        for (const name of toolDef.config.reserved) {
            result.reserved_parameters[name] = {
                source: "runtime",
                description: `Injected ${name}`
            };
        }
    }

    return successResponse(id, result);
}

/**
 * Handle tool/execute
 */
async function handleExecute(id, params, toolDef) {
    try {
        const args = params.args || {};
        const context = params.context || {};

        // Merge reserved params from context
        const mergedArgs = { ...args };
        for (const reserved of toolDef.config.reserved) {
            if (context[reserved] !== undefined) {
                mergedArgs[reserved] = context[reserved];
            }
        }

        // Call handler
        const result = await toolDef.handler(mergedArgs);

        // Format result
        let formattedResult;
        if (result === null || result === undefined) {
            formattedResult = { success: true };
        } else if (typeof result !== 'object') {
            formattedResult = { success: true, data: result };
        } else {
            formattedResult = { success: true, ...result };
            if (!formattedResult.hasOwnProperty('success')) {
                formattedResult.success = true;
            }
        }

        return successResponse(id, formattedResult);
    } catch (e) {
        console.error(e);
        return successResponse(id, {
            success: false,
            error: e.message
        });
    }
}

/**
 * Create success response
 */
function successResponse(id, result) {
    return {
        jsonrpc: "2.0",
        id,
        result
    };
}

/**
 * Create error response
 */
function errorResponse(id, code, message) {
    return {
        jsonrpc: "2.0",
        id,
        error: { code, message }
    };
}

module.exports = { tool, run };

// If run directly, start demo echo tool
if (require.main === module) {
    const echoTool = tool(
        {
            name: "echo",
            description: "Echoes back the input",
            parameters: {
                message: { type: "string", description: "Message to echo" }
            }
        },
        ({ message }) => {
            return { echo: message };
        }
    );
    
    run(echoTool);
}
