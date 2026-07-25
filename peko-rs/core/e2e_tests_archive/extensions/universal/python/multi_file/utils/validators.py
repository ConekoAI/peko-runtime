"""Input validation utilities."""


def validate_number(value, name="value"):
    """Validate that a value is a valid number."""
    try:
        return float(value), None
    except (ValueError, TypeError):
        return None, f"Invalid {name}: '{value}' is not a number"


def validate_operation(operation, allowed_ops):
    """Validate that operation is in allowed list."""
    if operation not in allowed_ops:
        return False, f"Invalid operation: '{operation}'. Allowed: {', '.join(allowed_ops)}"
    return True, None
