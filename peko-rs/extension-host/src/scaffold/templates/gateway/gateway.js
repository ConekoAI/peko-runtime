#!/usr/bin/env node
/**
 * Gateway process for {{name}}.
 *
 * Communicates with the Peko daemon via stdio-line JSON protocol.
 * Receives GatewayPacket on stdin, sends GatewayResponse on stdout.
 */

const readline = require('readline');

const rl = readline.createInterface({
  input: process.stdin,
  output: process.stdout,
  terminal: false,
});

function sendResponse(response) {
  console.log(JSON.stringify(response));
}

function handleConfig(gatewayId, routing) {
  console.error(`Received config for gateway ${gatewayId}`);
  // TODO: Initialize your gateway connection here
}

function handleDeliver(requestId, channelId, message, sessionId) {
  // TODO: Deliver the message to the appropriate platform channel
  sendResponse({
    type: 'delivered',
    request_id: requestId,
    message_id: null,
  });
}

function handlePing(requestId) {
  sendResponse({
    type: 'pong',
    request_id: requestId,
  });
}

function handleShutdown(requestId) {
  sendResponse({
    type: 'delivered',
    request_id: requestId,
    message_id: null,
  });
  process.exit(0);
}

function simulateReceive(channelId, userId, message) {
  sendResponse({
    type: 'receive',
    request_id: 0,
    channel_id: channelId,
    user_id: userId,
    message: message,
    metadata: {},
  });
}

console.error('Gateway starting...');

rl.on('line', (line) => {
  if (!line.trim()) return;

  let packet;
  try {
    packet = JSON.parse(line);
  } catch (e) {
    console.error(`Invalid JSON: ${line}`);
    return;
  }

  const packetType = packet.type;
  const requestId = packet.request_id || 0;

  switch (packetType) {
    case 'config':
      handleConfig(packet.gateway_id || '', packet.routing || {});
      break;
    case 'deliver':
      handleDeliver(
        requestId,
        packet.channel_id || '',
        packet.message || '',
        packet.session_id || ''
      );
      break;
    case 'ping':
      handlePing(requestId);
      break;
    case 'shutdown':
      handleShutdown(requestId);
      break;
    default:
      sendResponse({
        type: 'error',
        request_id: requestId,
        message: `Unknown packet type: ${packetType}`,
      });
  }
});
