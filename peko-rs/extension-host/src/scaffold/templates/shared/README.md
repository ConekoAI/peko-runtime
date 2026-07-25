# {{name}}

{{description}}

## Installation

```bash
peko ext install ./{{id}}
```

## Configuration

```bash
# View configuration
peko ext config {{id}} --show

# Set a configuration value
peko ext config {{id}} --set key=value
```

## Usage

Describe how to use this extension after installation.

## Development

```bash
# Validate the extension
peko ext validate ./{{id}}

# Grant for a Principal
peko capability grant --principal <principal-name> <capability>
```
