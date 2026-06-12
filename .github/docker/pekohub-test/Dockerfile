# PekoHub Test Fixture Server
# Self-contained container with PGlite (in-memory PostgreSQL) and mock storage/search.
# No external services needed.

FROM node:22-alpine AS builder
WORKDIR /app

RUN corepack enable && corepack prepare pnpm@9.0.0 --activate

# Copy workspace config
COPY package.json pnpm-workspace.yaml turbo.json pnpm-lock.yaml ./
COPY packages/shared/package.json packages/shared/
COPY backend/package.json backend/

RUN pnpm install --frozen-lockfile

# Copy source
COPY packages/shared/src packages/shared/src
COPY packages/shared/tsconfig.json packages/shared/
COPY backend/src backend/src
COPY backend/drizzle.config.ts backend/
COPY backend/drizzle backend/drizzle
COPY backend/tsconfig.json backend/
COPY backend/tests backend/tests

# Build
RUN pnpm --filter @pekohub/shared build
RUN pnpm --filter @pekohub/backend build

# ── Runtime stage ──
FROM node:22-alpine
WORKDIR /app

RUN corepack enable && corepack prepare pnpm@9.0.0 --activate

COPY package.json pnpm-workspace.yaml pnpm-lock.yaml ./
COPY packages/shared/package.json packages/shared/
COPY backend/package.json backend/
RUN pnpm install --frozen-lockfile --prod

# tsx is a devDependency but needed to run the fixture server TypeScript directly.
# Use `npm install -g` rather than `pnpm add -g`: pnpm 9+ on the minimal
# Alpine image doesn't have a default global bin directory and refuses
# to install unless PNPM_HOME is explicitly set + PATH-exported. npm
# is always present on the node base image and always knows where
# its global bin dir is.
RUN npm install -g tsx@4.15.0

COPY --from=builder /app/packages/shared/dist packages/shared/dist
COPY --from=builder /app/backend/src backend/src
COPY --from=builder /app/backend/dist backend/dist
COPY --from=builder /app/backend/drizzle backend/drizzle
COPY --from=builder /app/backend/drizzle.config.ts backend/
COPY --from=builder /app/backend/tests backend/tests

# Test fixture environment (no external services needed)
ENV NODE_ENV=development
ENV JWT_SECRET=test-secret-key-that-is-32-chars-long!!
ENV ALLOW_DEV_AUTH_BYPASS=true
ENV GC_ENABLED=false
ENV RATE_LIMIT_MAX=1000
ENV DATABASE_URL=postgres://localhost:5432/pekohub_test
ENV S3_ENDPOINT=http://localhost:9000
ENV S3_ACCESS_KEY=test
ENV S3_SECRET_KEY=test
ENV S3_BUCKET=test-bucket
ENV MEILISEARCH_URL=http://localhost:7700
ENV MEILISEARCH_API_KEY=test

EXPOSE 3000

# The test fixture server auto-starts PGlite, creates tables, listens.
# Invoke `tsx` directly rather than `node --import tsx …`: globally-
# installed `tsx` lives at /usr/local/bin/tsx, but `node --import tsx`
# uses Node's ESM resolver, which does NOT search globally-installed
# modules (NODE_PATH only affects CommonJS resolution). The `tsx`
# binary is on the default PATH so `tsx file.ts` Just Works.
CMD ["tsx", "backend/tests/fixtures/server.ts", "--port", "3000"]
