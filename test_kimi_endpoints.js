#!/usr/bin/env node
/**
 * Kimi Code - Find Working Endpoint
 * Try various potential endpoints
 */

const fs = require('fs');
const path = require('path');

async function main() {
    console.log("🧪 Finding Kimi Code Endpoint");
    console.log("=============================\n");

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

    const cleanKey = apiKey.replace(/^sk-kimi-/, '').replace(/^sk-/, '');
    console.log(`API Key (cleaned): ${cleanKey.substring(0, 15)}...${cleanKey.substring(cleanKey.length - 8)}\n`);

    // Various endpoints to try
    const tests = [
        // OpenAI format endpoints
        { url: 'https://api.moonshot.cn/v1/chat/completions', format: 'openai', key: apiKey },
        { url: 'https://api.moonshot.ai/v1/chat/completions', format: 'openai', key: apiKey },
        
        // Anthropic format endpoints (Kimi Code uses Claude backend)
        { url: 'https://api.moonshot.cn/v1/messages', format: 'anthropic', key: cleanKey },
        { url: 'https://api.moonshot.ai/v1/messages', format: 'anthropic', key: cleanKey },
        { url: 'https://api.anthropic.com/v1/messages', format: 'anthropic', key: cleanKey },
        
        // Potential Kimi Code specific endpoints
        { url: 'https://kimi-code.moonshot.cn/v1/messages', format: 'anthropic', key: cleanKey },
        { url: 'https://kimi-code.moonshot.cn/v1/chat/completions', format: 'openai', key: apiKey },
        { url: 'https://code.moonshot.cn/v1/messages', format: 'anthropic', key: cleanKey },
        
        // Try with Bearer auth for anthropic format too
        { url: 'https://api.moonshot.cn/v1/messages', format: 'bearer', key: apiKey },
    ];

    for (const test of tests) {
        console.log(`🔍 ${test.format.toUpperCase()} → ${test.url.replace('https://', '')}`);
        
        const headers = { 'Content-Type': 'application/json' };
        let body = {};

        if (test.format === 'openai' || test.format === 'bearer') {
            headers['Authorization'] = `Bearer ${test.key}`;
            body = {
                model: 'kimi-k2.5',
                max_tokens: 100,
                messages: [{ role: 'user', content: 'Say hello' }]
            };
        } else { // anthropic
            headers['x-api-key'] = test.key;
            headers['anthropic-version'] = '2023-06-01';
            body = {
                model: 'claude-3-haiku-20240307',  // Try Claude model
                max_tokens: 100,
                messages: [{ role: 'user', content: 'Say hello' }]
            };
        }

        try {
            const response = await fetch(test.url, {
                method: 'POST',
                headers,
                body: JSON.stringify(body),
            });

            const responseBody = await response.text();
            
            if (response.ok) {
                console.log(`✅ SUCCESS!\n`);
                try {
                    const json = JSON.parse(responseBody);
                    console.log(`📝 Response: ${json.content?.[0]?.text || json.choices?.[0]?.message?.content || 'OK'}`);
                } catch (e) {
                    console.log(`📄 Response: ${responseBody.substring(0, 200)}`);
                }
                console.log(`\n🎉 Working config:`);
                console.log(`   URL: ${test.url}`);
                console.log(`   Format: ${test.format}`);
                console.log(`   Key format: ${test.key === apiKey ? 'original' : 'cleaned'}`);
                return;
            } else {
                const msg = responseBody.length > 100 
                    ? responseBody.substring(0, 100) + '...' 
                    : responseBody;
                console.log(`❌ ${response.status}: ${msg}\n`);
            }
        } catch (error) {
            console.log(`❌ ${error.message}\n`);
        }
    }

    console.log("❌ No working endpoint found!");
    console.log("\n💡 The API key may be invalid or Kimi Code uses a different endpoint.");
}

main().catch(console.error);
