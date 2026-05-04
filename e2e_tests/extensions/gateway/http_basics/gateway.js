#!/usr/bin/env node
/**
 * Reference HTTP Gateway — Minimal out-of-process gateway for E2E testing.
 *
 * Protocol: stdio-line JSON (one JSON object per line, newline-delimited)
 * Direction:
 *   - stdin:  Receives GatewayPacket from daemon
 *   - stdout: Sends GatewayResponse to daemon
 *
 * This gateway does NOT open an HTTP server. Instead, it simulates the
 * behavior of a real gateway by:
 *   1. Reading config from daemon
 *   2. Responding to pings
 *   3. After a short delay, emitting a synthetic "Receive" message
 *      (as if a user sent a message via HTTP POST)
 *   4. Receiving the agent response via Deliver and logging it
 *   5. Responding to graceful shutdown
 */

const fs = require('fs');
const path = require('path');
const readline = require('readline');

const logFile = path.join(__dirname, 'gateway_debug.log');
function log(msg) {
    const line = `[${new Date().toISOString()}] ${msg}\n`;
    fs.appendFileSync(logFile, line);
    console.error(line.trim());
}

let requestIdCounter = 1;
let gatewayId = null;
let routingConfig = null;
let shutdownRequested = false;

function nextRequestId() {
    return requestIdCounter++;
}

function sendResponse(obj) {
    const line = JSON.stringify(obj);
    log(`SEND: ${line}`);
    process.stdout.write(line + '\n');
}

function handleConfig(packet) {
    gatewayId = packet.gateway_id;
    routingConfig = packet.routing;
    log(`Config received. Default agent: ${routingConfig?.default_agent || '(none)'}`);

    // Simulate an incoming message after a short delay
    // This is the "HTTP POST" equivalent — a user sent a message
    setTimeout(() => {
        if (shutdownRequested) return;
        const reqId = nextRequestId();
        sendResponse({
            type: 'receive',
            request_id: reqId,
            channel_id: 'test-channel',
            user_id: 'test-user',
            message: 'Hello from HTTP gateway reference implementation',
            metadata: { source: 'http_gateway_e2e', simulated: true }
        });
        log(`Simulated incoming message (req ${reqId})`);
    }, 500);
}

function handlePing(packet) {
    log(`Ping received (req ${packet.request_id})`);
    sendResponse({
        type: 'pong',
        request_id: packet.request_id
    });
}

function handleDeliver(packet) {
    log(`Deliver received for channel ${packet.channel_id}: ${packet.message}`);
    sendResponse({
        type: 'delivered',
        request_id: packet.request_id,
        message_id: `msg-${packet.request_id}`
    });
}

function handleShutdown(packet) {
    log(`Shutdown requested (req ${packet.request_id})`);
    shutdownRequested = true;
    sendResponse({
        type: 'delivered', // ack the shutdown
        request_id: packet.request_id,
        message_id: null
    });
    // Give stdout time to flush before exiting
    clearInterval(keepAlive);
    setTimeout(() => process.exit(0), 100);
}

function handlePacket(line) {
    let packet;
    try {
        packet = JSON.parse(line.trim());
    } catch (e) {
        console.error(`[gateway] Failed to parse packet: ${e.message}`);
        return;
    }

    switch (packet.type) {
        case 'config':
            handleConfig(packet);
            break;
        case 'ping':
            handlePing(packet);
            break;
        case 'deliver':
            handleDeliver(packet);
            break;
        case 'shutdown':
            handleShutdown(packet);
            break;
        default:
            log(`Unknown packet type: ${packet.type}`);
    }
}

// Main read loop
const rl = readline.createInterface({
    input: process.stdin,
    terminal: false
});

rl.on('line', (line) => {
    if (!line.trim()) return;
    handlePacket(line);
});

rl.on('close', () => {
    log('stdin closed, exiting');
    process.exit(0);
});

// Keep the event loop alive so the process doesn't exit before timeouts fire
const keepAlive = setInterval(() => {}, 10000);

log('Reference HTTP gateway started, waiting for config...');
