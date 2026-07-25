#!/usr/bin/env node
/**
 * Handler for {{name}} universal tool extension.
 *
 * Receives JSON on stdin, outputs JSON on stdout.
 * Expected input format:
 *     {"input": "..."}
 *
 * Output format:
 *     {"result": "...", "error": null}
 */

const readline = require('readline');

const rl = readline.createInterface({
  input: process.stdin,
  output: process.stdout,
  terminal: false,
});

rl.on('line', (line) => {
  try {
    const request = JSON.parse(line);
    const userInput = request.input || '';

    // TODO: Implement your tool logic here
    const result = `Processed: ${userInput}`;

    const response = { result, error: null };
    console.log(JSON.stringify(response));
  } catch (e) {
    const response = { result: null, error: e.message };
    console.log(JSON.stringify(response));
    process.exit(1);
  }
});
