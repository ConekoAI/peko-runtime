# Multi-File Tool E2E Test

This E2E test verifies that Pekobot properly handles tools with multi-file structures, including subdirectories.

## Purpose

Demonstrates and tests:
1. **Recursive directory copy** during tool installation
2. **Python import from subdirectories** works correctly
3. **Helper modules** can be organized in `utils/` subdirectory

## Tool Structure

```
multi_file/
├── multi_file_calc.py      # Main executable
├── multi_file_calc.json    # Manifest
├── utils/                  # Subdirectory with helper modules
│   ├── __init__.py
│   ├── calculator.py       # Math operations
│   ├── validators.py       # Input validation
│   └── formatter.py        # Result formatting
└── test.ps1               # E2E test script
```

## Key Features Tested

| Feature | File(s) |
|---------|---------|
| Subdirectory preservation | `utils/` directory |
| Module imports | `utils/calculator.py`, `utils/validators.py` |
| Multiple helper files | All files in `utils/` |
| End-to-end functionality | Full calculation via agent |

## Running the Test

```powershell
# Set API key
$env:KIMI_API_KEY = "your-api-key"

# Run the test
cd e2e_tests/cap/tool/custom/python/multi_file
.\test.ps1
```

## Expected Output

```
========================================
Multi-File Tool E2E Test
========================================
Using Python: python3
...
✓ All files including subdirectory contents installed
...
Agent response: ... 15 × 6 = 90 ...
✓ Tool returned correct result (15 × 6 = 90)
✅ Multi-file tool E2E test completed successfully!
```

## Manual Verification

You can also test manually:

```bash
# Install the tool
pekobot cap universal install ./multi_file --force

# Create agent and enable tool
pekobot agent create calc --provider kimi
pekobot cap enable default/calc multi_file_calc

# Use it
pekobot send calc "Calculate 10 + 20 using multi_file_calc"
```

## Import Mechanism

The main file adds the tool directory to Python path:

```python
import sys
import os
sys.path.insert(0, os.path.dirname(__file__))

from utils.calculator import add, subtract, multiply, divide
from utils.validators import validate_number, validate_operation
from utils.formatter import format_result, format_error
```

This ensures the imports work regardless of where the tool is installed.
