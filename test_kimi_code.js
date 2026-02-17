#!/usr/bin/env node
/**
 * Kimi Code API Test
 * Tests various endpoints to find the working Kimi Code API
 */

const fs = require('fs');
const path = require('path');

async function main() {
    console.log("🧪 Kimi Code Provider Test");
    console.log("==========================\n");

    // Load API key from auth profiles
    const homeDir = process.env.HOME || '/home/ubuntu';
    const authFilePath = path.join(homeDir, '.openclaw/agents/main/agent/auth-profiles.json');
    
    let apiKey = null;
    try {
        const authContent = fs.readFileSync(authFilePath, 'utf8');
        const authJson = JSON.parse(authContent);
        apiKey = authJson.profiles?.['kimi-coding:default']?.key;
    } catch (e) {
        console.log("Could not read auth profiles, trying env var...");
    }

    if (!apiKey) {
        apiKey = process.env.KIMI_API_KEY;
    }

    if (!apiKey) {
        console.error("❌ No API key found!");
        console.error("Set KIMI_API_KEY or update auth-profiles.json");
        process.exit(1);
    }

    console.log(`✅ API key loaded (length: ${apiKey.length})\n`);

    // Strip kimi- prefix if present for auth
    const cleanKey = apiKey.replace(/^kimi-/, '');
    
    // Test endpoints
    const endpoints = [
        { url: 'https://api.kimi-code.moonshot.cn/v1/messages', name: 'Kimi Code (kimi-code.moonshot.cn)' },
        { url: 'https://api.moonshot.cn/v1/messages', name: 'Moonshot Messages (moonshot.cn)' },
        { url: 'https://api.moonshot.cn/v1/chat/completions', name: 'Moonshot CN Chat (OpenAI format)' },
        { url: 'https://api.moonshot.ai/v1/chat/completions', name: 'Moonshot AI Chat (OpenAI format)' },
        { url: 'https://api.moonshot.ai/v1/messages', name: 'Moonshot AI Messages (Anthropic format)' },
    ];

    const requestBody = {
        model: 'kimi-k2.5',
        max_tokens: 1024,
        temperature: 0.7,
        messages: [
            { role: 'user', content: 'Say "Hello from Pekobot!" and nothing else.' }
        ]
    };

    for (const endpoint of endpoints) {
        console.log(`🔍 Testing: ${endpoint.name}`);
        console.log(`   URL: ${endpoint.url}`);
        
        try {
            const isAnthropicFormat = endpoint.url.includes('/messages');
            const headers = {
                'Content-Type': 'application/json',
            };
            
            if (isAnthropicFormat) {
                headers['x-api-key'] = cleanKey;
                headers['anthropic-version'] = '2023-06-01';
            } else {
                headers['Authorization'] = `Bearer ${apiKey}`;
            }

            const response = await fetch(endpoint.url, {
                method: 'POST',
                headers,
                body: JSON.stringify(requestBody),
            });

            const body = await response.text();
            
            if (response.ok) {
                console.log(`✅ SUCCESS! Status: ${response.status}\n`);
                try {
                    const json = JSON.parse(body);
                    if (json.content && json.content[0]?.text) {
                        console.log(`📝 Response: ${json.content[0].text.trim()}\n`);
                    } else if (json.choices && json.choices[0]?.message?.content) {
                        console.log(`📝 Response: ${json.choices[0].message.content.trim()}\n`);
                    } else {
                        console.log(`📄 Response:\n${JSON.stringify(json, null, 2).substring(0, 500)}\n`);
                    }
                } catch (e) {
                    console.log(`📄 Raw response:\n${body.substring(0, 500)}\n`);
                }
                
                // Found working endpoint!
                console.log(`🎉 Working endpoint found: ${endpoint.name}`);
                console.log(`   Format: ${isAnthropicFormat ? 'Anthropic' : 'OpenAI'}`);
                return;
            } else {
                console.log(`❌ FAILED! Status: ${response.status}`);
                console.log(`   Error: ${body.substring(0, 200)}\n`);
            }
        } catch (error) {
            console.log(`❌ Error: ${error.message}\n`);
        }
    }

    console.error("❌ All endpoints failed!");
    process.exit(1);
}

main().catch(err => {
    console.error("Fatal error:", err);
    process.exit(1);
});
