#!/usr/bin/env node
/**
 * Kimi Code API Test - Key Variations
 * Tests different key formats
 */

const fs = require('fs');
const path = require('path');

async function main() {
    console.log("🧪 Kimi Code Key Format Test");
    console.log("=============================\n");

    // Load API key from auth profiles
    const homeDir = process.env.HOME || '/home/ubuntu';
    const authFilePath = path.join(homeDir, '.openclaw/agents/main/agent/auth-profiles.json');
    
    let apiKey = null;
    try {
        const authContent = fs.readFileSync(authFilePath, 'utf8');
        const authJson = JSON.parse(authContent);
        apiKey = authJson.profiles?.['kimi-coding:default']?.key;
    } catch (e) {
        console.log("Could not read auth profiles");
    }

    if (!apiKey) {
        console.error("❌ No API key found!");
        process.exit(1);
    }

    console.log(`Original key: ${apiKey.substring(0, 20)}...${apiKey.substring(apiKey.length - 8)}`);
    console.log(`Key starts with "sk-kimi-": ${apiKey.startsWith('sk-kimi-')}`);
    console.log(`Key length: ${apiKey.length}\n`);

    // Try different key variations
    const keyVariations = [
        { name: 'Original (sk-kimi-...)', key: apiKey },
        { name: 'Without sk- prefix', key: apiKey.replace(/^sk-/, '') },
        { name: 'Without kimi- prefix', key: apiKey.replace(/^sk-kimi-/, 'sk-') },
        { name: 'Just the key (no prefixes)', key: apiKey.replace(/^sk-kimi-/, '') },
    ];

    const endpoint = 'https://api.moonshot.cn/v1/chat/completions';
    const requestBody = {
        model: 'kimi-k2.5',
        max_tokens: 100,
        messages: [{ role: 'user', content: 'Hi' }]
    };

    for (const variation of keyVariations) {
        console.log(`🔍 Testing: ${variation.name}`);
        console.log(`   Key: ${variation.key.substring(0, 20)}...${variation.key.substring(variation.key.length - 8)}`);
        
        try {
            const response = await fetch(endpoint, {
                method: 'POST',
                headers: {
                    'Authorization': `Bearer ${variation.key}`,
                    'Content-Type': 'application/json',
                },
                body: JSON.stringify(requestBody),
            });

            const body = await response.text();
            
            if (response.ok) {
                console.log(`✅ SUCCESS!\n`);
                try {
                    const json = JSON.parse(body);
                    console.log(`📝 Response: ${json.choices?.[0]?.message?.content?.trim() || 'OK'}\n`);
                } catch (e) {
                    console.log(`📄 Raw: ${body.substring(0, 200)}\n`);
                }
                return;
            } else {
                console.log(`❌ ${response.status}: ${body.substring(0, 100)}\n`);
            }
        } catch (error) {
            console.log(`❌ Error: ${error.message}\n`);
        }
    }

    console.log("❌ All key variations failed!");
    console.log("\n💡 The API key appears to be invalid or expired.");
    console.log("   Get a fresh key from https://platform.moonshot.cn/");
    process.exit(1);
}

main().catch(console.error);
