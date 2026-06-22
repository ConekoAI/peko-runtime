#!/usr/bin/env python3
"""
Gateway process for {{name}}.

Communicates with the Pekobot daemon via stdio-line JSON protocol.
Receives GatewayPacket on stdin, sends GatewayResponse on stdout.
"""

import json
import sys
import threading
import time


def send_response(response: dict):
    """Send a JSON response line to stdout."""
    print(json.dumps(response), flush=True)


def handle_config(gateway_id: str, routing: dict):
    """Handle initial config packet from daemon."""
    print(f"Received config for gateway {gateway_id}", file=sys.stderr)
    # TODO: Initialize your gateway connection here


def handle_deliver(request_id: int, channel_id: str, message: str, session_id: str):
    """Handle an outgoing message from the agent."""
    # TODO: Deliver the message to the appropriate platform channel
    send_response({
        "type": "delivered",
        "request_id": request_id,
        "message_id": None,
    })


def handle_ping(request_id: int):
    """Respond to health check ping."""
    send_response({
        "type": "pong",
        "request_id": request_id,
    })


def handle_shutdown(request_id: int):
    """Handle graceful shutdown request."""
    send_response({
        "type": "delivered",
        "request_id": request_id,
        "message_id": None,
    })
    sys.exit(0)


def simulate_receive(channel_id: str, user_id: str, message: str):
    """Simulate an incoming message from a user."""
    send_response({
        "type": "receive",
        "request_id": 0,
        "channel_id": channel_id,
        "user_id": user_id,
        "message": message,
        "metadata": {},
    })


def main():
    print("Gateway starting...", file=sys.stderr)

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue

        try:
            packet = json.loads(line)
        except json.JSONDecodeError:
            print(f"Invalid JSON: {line}", file=sys.stderr)
            continue

        packet_type = packet.get("type")
        request_id = packet.get("request_id", 0)

        if packet_type == "config":
            handle_config(packet.get("gateway_id", ""), packet.get("routing", {}))
        elif packet_type == "deliver":
            handle_deliver(
                request_id,
                packet.get("channel_id", ""),
                packet.get("message", ""),
                packet.get("session_id", ""),
            )
        elif packet_type == "ping":
            handle_ping(request_id)
        elif packet_type == "shutdown":
            handle_shutdown(request_id)
        else:
            send_response({
                "type": "error",
                "request_id": request_id,
                "message": f"Unknown packet type: {packet_type}",
            })


if __name__ == "__main__":
    main()
