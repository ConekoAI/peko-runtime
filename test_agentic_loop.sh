#!/bin/bash
export KIMI_API_KEY=$(grep "export KIMI_API_KEY=" ~/.bashrc | head -1 | sed 's/.*export KIMI_API_KEY="\(.*\)".*/\1/')
source "$HOME/.cargo/env" && cargo build --bin pekobot 2>&1 | tail -1
rm -rf ~/.pekobot
./target/debug/pekobot agent create testagent --yes
./target/debug/pekobot agent start testagent -M "Feed me some news"
