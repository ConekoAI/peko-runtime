---
name: docker
description: Docker container and image management - build, run, stop, and manage containers
tags: [devops, containers]
author: Pekobot
---

# Docker Skill

Use this skill when working with Docker containers and images.

## When to Use

✅ **Use this skill for:**
- Building Docker images
- Running containers
- Stopping/removing containers
- Viewing container logs
- Managing Docker volumes and networks

❌ **Don't use for:**
- Kubernetes deployments → use `kubectl` directly
- Docker Compose multi-container apps → use `docker compose`

## Common Commands

### Build an Image

```bash
docker build -t myapp:latest .
```

### Run a Container

```bash
# Run in background
docker run -d --name myapp -p 8080:80 myapp:latest

# Run interactively
docker run -it --rm myapp:latest /bin/sh
```

### View Running Containers

```bash
docker ps
docker ps -a  # Include stopped containers
```

### View Logs

```bash
docker logs myapp
docker logs -f myapp  # Follow logs
```

### Stop and Remove

```bash
docker stop myapp
docker rm myapp
# Or in one command:
docker rm -f myapp
```

### Clean Up

```bash
# Remove unused images
docker image prune

# Remove unused volumes
docker volume prune

# Full cleanup (careful!)
docker system prune
```

## Best Practices

1. Always use `-d` for long-running services
2. Use `--rm` for one-off commands
3. Tag images with versions, not just `latest`
4. Clean up stopped containers regularly
