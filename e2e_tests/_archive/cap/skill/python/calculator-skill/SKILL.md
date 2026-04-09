---
name: calculator-skill
description: Perform arithmetic calculations with clear step-by-step explanations
tags: [math, calculator, arithmetic]
author: Pekobot E2E Test
---

# Calculator Skill

## Instructions

When the user asks for calculations:
1. Identify the arithmetic operation needed (add, subtract, multiply, divide)
2. Perform the calculation accurately
3. Provide the result with a clear explanation
4. Show your work step-by-step when helpful

## Operations Supported

- **Addition** (+): Combine two or more numbers
- **Subtraction** (-): Find the difference between numbers
- **Multiplication** (*): Calculate the product of numbers
- **Division** (/): Divide one number by another

## Output Format

```
Operation: [operation name]
Expression: [the math expression]
Result: [the answer]
Explanation: [brief explanation]
```

## Examples

User: "What's 25 times 4?"
→ Operation: Multiplication
→ Expression: 25 × 4
→ Result: 100
→ Explanation: 25 multiplied by 4 equals 100

User: "Calculate 100 divided by 3"
→ Operation: Division
→ Expression: 100 ÷ 3
→ Result: 33.33 (repeating)
→ Explanation: 100 divided by 3 is approximately 33.33

## Guidelines

- Always double-check your calculations
- Format numbers clearly (use commas for thousands if helpful)
- For division, mention if the result is repeating or approximate
- Offer to perform additional calculations if needed
