#!/bin/bash
set -e

cd ~/pekora/projects/pekobot
./test_scripts/common/init_kimi.sh


echo "========================================"
echo "Test: Session Resumption"
echo "========================================"
echo "Prompt: 'What's USA's Capital?'"
echo ""
pekobot agent start testagent -M "What's USA's Capital?"
echo "Prompt: 'What about France?'"
echo ""
pekobot agent start testagent -M "What about France?"
echo ""
echo "========================================"
echo "Test completed!"
echo "========================================"
