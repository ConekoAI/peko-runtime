"""Result formatting utilities."""


def format_result(operation, a, b, result):
    """Format calculation result nicely."""
    symbols = {
        "add": "+",
        "subtract": "-",
        "multiply": "×",
        "divide": "÷"
    }
    symbol = symbols.get(operation, operation)
    return f"{a} {symbol} {b} = {result}"


def format_error(error_msg):
    """Format error message."""
    return f"Error: {error_msg}"
