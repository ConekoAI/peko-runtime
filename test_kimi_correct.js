#!/usr/bin/env node
/**
 * Kimi Code API Test - Correct Endpoint
 * Uses https://api.kimi.com/coding (found in pi-mono)
 */

const fs = require('fs');
const path = require('path');

async function main() {
    console.log("🧪 Kimi Code API Test (pi-mono endpoint)");
    console.log("========================================\n");

    const homeDir = process.env.HOME || '/home/ubuntu';
    const authFilePath = path.join(homeDir, '.openclaw/agents/main/agent/auth-profiles.json');
    
    let apiKey = null;
    try {
        const authContent = fs.readFileSync(authFilePath, 'utf8');
        const authJson = JSON.parse(authContent);
        apiKey = authJson.profiles?.['kimi-coding:default']?.key;
    } catch (e) {
        console.error("Could not read auth profiles");
        process.exit(1);
    }

    if (!apiKey) {
        console.error("❌ No API key found!");
        process.exit(1);
    }

    console.log(`API Key: ${apiKey.substring(0, 20)}...${apiKey.substring(apiKey.length - 8)}`);
    console.log(`Key starts with "sk-kimi-": ${apiKey.startsWith('sk-kimi-')}\n`);

    // The correct endpoint from pi-mono
    const endpoint = 'https://api.kimi.com/coding/v1/messages';
    
    console.log(`🔍 Testing endpoint: ${endpoint}\n`);

    // Anthropic format request
    const requestBody = {
        model: 'k2p5',
        max_tokens: 1024,
        temperature: 0.7,
        messages: [
            { role: 'user', content: 'Say "Hello from Pekobot!" and nothing else.' }
        ]
    };

    try {
        const response = await fetch(endpoint, {
            method: 'POST',
            headers: {
                'Content-Type': 'application/json',
                'x-api-key': apiKey,
                'anthropic-version': '2023-06-01',
            },
            body: JSON.stringify(requestBody),
        });

        const body = await response.text();
        
        if (response.ok) {
            console.log(`✅ SUCCESS! Status: ${response.status}\n`);
            try {
                const json = JSON.parse(body);
                if (json.content && json.content[0]?.text) {
                    console.log(`📝 Response: ${json.content[0].text.trim()}\n`);
                } else {
                    console.log(`📄 Response:\n${JSON.stringify(json, null, 2).substring(0, 500)}\n`);
                }
            } catch (e) {
                console.log(`📄 Raw response:\n${body.substring(0, 500)}\n`);
            }
            
            console.log("🎉 Kimi Code is working!");
            console.log("   Endpoint: https://api.kimi.com/coding/v1/messages");
            console.log("   Format: Anthropic (x-api-key header)");
        } else {
            console.log(`❌ FAILED! Status: ${response.status}`);
            console.log(`   Error: ${body.substring(0, 300)}\n`);
            
            // Try with cleaned key
            console.log("🔍 Retrying with cleaned key (no 'sk-kimi-' prefix)...");
            const cleanKey = apiKey.replace(/^sk-kimi-/, '');
            
            const retryResponse = await fetch(endpoint, {
                method: 'POST',
                headers: {
                    'Content-Type': 'application/json',
                    'x-api-key': cleanKey,
                    'anthropic-version': '2023-06-01',
                },
                body: JSON.stringify(requestBody),
            });
            
            const retryBody = await retryResponse.text();
            if (retryResponse.ok) {
                console.log(`✅ SUCCESS with cleaned key!\n`);
                const json = JSON.parse(retryBody);
                console.log(`📝 Response: ${json.content?.[0]?.text?.trim() || 'OK'}\n`);
            } else {
                console.log(`❌ Also failed: ${retryResponse.status} - ${retryBody.substring(0, 200)}\n`);
            }
        }
    } catch (error) {
        console.log(`❌ Error: ${error.message}\n`);
    }
}

main().catch(console.error);
