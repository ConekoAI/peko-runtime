#!/usr/bin/env python3
"""Pekobot Mock Registry Server

A lightweight FastAPI-based mock registry implementing an OCI-inspired
protocol for .agent package push/pull. Used for integration testing.

Endpoints:
    GET    /v2/                          → 200 (capability check)
    GET    /v2/{name}/manifests/{ref}    → manifest JSON
    PUT    /v2/{name}/manifests/{ref}    → store manifest
    HEAD   /v2/{name}/blobs/{digest}     → 200 if exists, 404 if not
    GET    /v2/{name}/blobs/{digest}     → layer bytes
    POST   /v2/{name}/blobs/uploads/     → 202 + upload URL
    PUT    /v2/{name}/blobs/uploads/{uuid} → complete upload

Storage:
    In-memory by default. Optionally file-backed via --storage-dir.

Usage:
    python main.py --port 0 --host 127.0.0.1
    python main.py --storage-dir ./registry-data
"""

from __future__ import annotations

import argparse
import hashlib
import io
import json
import os
import sys
import uuid
from pathlib import Path
from typing import Any

from fastapi import FastAPI, HTTPException, Request, Response
from fastapi.responses import JSONResponse, PlainTextResponse

app = FastAPI(title="Pekobot Mock Registry")

# ---------------------------------------------------------------------------
# Storage backends
# ---------------------------------------------------------------------------

class MemoryStorage:
    """In-memory blob and manifest storage."""

    def __init__(self) -> None:
        self.blobs: dict[str, bytes] = {}          # digest -> bytes
        self.manifests: dict[str, str] = {}        # "repo:tag" -> manifest_json
        self.tags: dict[str, str] = {}             # "repo:tag" -> digest

    def has_blob(self, digest: str) -> bool:
        return digest in self.blobs

    def get_blob(self, digest: str) -> bytes | None:
        return self.blobs.get(digest)

    def put_blob(self, digest: str, data: bytes) -> None:
        self.blobs[digest] = data

    def get_manifest(self, repo: str, ref: str) -> str | None:
        key = f"{repo}:{ref}"
        return self.manifests.get(key)

    def put_manifest(self, repo: str, ref: str, data: str) -> None:
        key = f"{repo}:{ref}"
        self.manifests[key] = data
        # Also store by digest if ref looks like a digest
        if ref.startswith("sha256:"):
            self.manifests[f"{repo}:{ref}"] = data

    def get_tag_digest(self, repo: str, tag: str) -> str | None:
        return self.tags.get(f"{repo}:{tag}")

    def set_tag(self, repo: str, tag: str, digest: str) -> None:
        self.tags[f"{repo}:{tag}"] = digest


class FileStorage:
    """File-backed blob and manifest storage."""

    def __init__(self, base_dir: Path) -> None:
        self.base_dir = base_dir
        self.blobs_dir = base_dir / "blobs"
        self.manifests_dir = base_dir / "manifests"
        self.tags_dir = base_dir / "tags"
        for d in (self.blobs_dir, self.manifests_dir, self.tags_dir):
            d.mkdir(parents=True, exist_ok=True)

    def _blob_path(self, digest: str) -> Path:
        hex_part = digest.removeprefix("sha256:")
        return self.blobs_dir / f"sha256-{hex_part}.bin"

    def _manifest_path(self, repo: str, ref: str) -> Path:
        safe_repo = repo.replace("/", "_")
        return self.manifests_dir / f"{safe_repo}_{ref}.json"

    def _tag_path(self, repo: str, tag: str) -> Path:
        safe_repo = repo.replace("/", "_")
        return self.tags_dir / f"{safe_repo}_{tag}.txt"

    def has_blob(self, digest: str) -> bool:
        return self._blob_path(digest).exists()

    def get_blob(self, digest: str) -> bytes | None:
        path = self._blob_path(digest)
        if not path.exists():
            return None
        return path.read_bytes()

    def put_blob(self, digest: str, data: bytes) -> None:
        path = self._blob_path(digest)
        path.write_bytes(data)

    def get_manifest(self, repo: str, ref: str) -> str | None:
        path = self._manifest_path(repo, ref)
        if not path.exists():
            return None
        return path.read_text()

    def put_manifest(self, repo: str, ref: str, data: str) -> None:
        path = self._manifest_path(repo, ref)
        path.write_text(data)

    def get_tag_digest(self, repo: str, tag: str) -> str | None:
        path = self._tag_path(repo, tag)
        if not path.exists():
            return None
        return path.read_text().strip()

    def set_tag(self, repo: str, tag: str, digest: str) -> None:
        path = self._tag_path(repo, tag)
        path.write_text(digest)


# Global storage — set at startup
storage: MemoryStorage | FileStorage = MemoryStorage()

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _normalize_digest(digest: str) -> str:
    """Ensure digest has sha256: prefix."""
    if ":" not in digest:
        return f"sha256:{digest}"
    return digest

# ---------------------------------------------------------------------------
# Routes
# ---------------------------------------------------------------------------

@app.get("/v2/")
async def registry_check() -> Response:
    """Registry capability check."""
    return Response(status_code=200)


@app.head("/v2/{name:path}/blobs/{digest}")
async def check_blob(name: str, digest: str) -> Response:
    """Check if a blob (layer) exists."""
    digest = _normalize_digest(digest)
    if storage.has_blob(digest):
        return Response(
            status_code=200,
            headers={
                "Content-Length": str(len(storage.get_blob(digest) or b"")),
                "Docker-Content-Digest": digest,
            },
        )
    return Response(status_code=404)


@app.get("/v2/{name:path}/blobs/{digest}")
async def get_blob(name: str, digest: str) -> Response:
    """Download a blob (layer)."""
    digest = _normalize_digest(digest)
    data = storage.get_blob(digest)
    if data is None:
        raise HTTPException(status_code=404, detail=f"Blob not found: {digest}")
    return Response(
        content=data,
        media_type="application/octet-stream",
        headers={"Docker-Content-Digest": digest},
    )


@app.post("/v2/{name:path}/blobs/uploads/")
async def initiate_upload(name: str, request: Request) -> Response:
    """Initiate a blob upload — returns upload URL."""
    upload_id = str(uuid.uuid4())
    # Build absolute upload URL
    base_url = str(request.base_url).rstrip("/")
    upload_url = f"{base_url}/v2/{name}/blobs/uploads/{upload_id}"
    return Response(
        status_code=202,
        headers={
            "Location": upload_url,
            "Range": "0-0",
        },
    )


@app.put("/v2/{name:path}/blobs/uploads/{upload_id}")
async def complete_upload(
    name: str,
    upload_id: str,
    request: Request,
    digest: str | None = None,
) -> Response:
    """Complete a blob upload."""
    body = await request.body()

    # Compute digest from body if not provided in query
    if digest is None or not digest:
        computed = hashlib.sha256(body).hexdigest()
        digest = f"sha256:{computed}"
    else:
        digest = _normalize_digest(digest)

    storage.put_blob(digest, body)

    base_url = str(request.base_url).rstrip("/")
    blob_url = f"{base_url}/v2/{name}/blobs/{digest}"
    return Response(
        status_code=201,
        headers={
            "Location": blob_url,
            "Docker-Content-Digest": digest,
        },
    )


@app.get("/v2/{name:path}/manifests/{reference}")
async def get_manifest(name: str, reference: str, request: Request) -> Response:
    """Pull a manifest by tag or digest."""
    # Try direct lookup first
    data = storage.get_manifest(name, reference)
    if data is None:
        # Try resolving tag to digest, then lookup by digest
        tag_digest = storage.get_tag_digest(name, reference)
        if tag_digest:
            data = storage.get_manifest(name, tag_digest)

    if data is None:
        raise HTTPException(
            status_code=404, detail=f"Manifest not found: {name}:{reference}"
        )

    return Response(
        content=data,
        media_type="application/vnd.pekobot.manifest.v1+json",
    )


@app.put("/v2/{name:path}/manifests/{reference}")
async def put_manifest(name: str, reference: str, request: Request) -> Response:
    """Push a manifest."""
    body = await request.body()
    data = body.decode("utf-8")

    # Compute manifest digest
    computed = hashlib.sha256(body).hexdigest()
    digest = f"sha256:{computed}"

    # Store by tag
    storage.put_manifest(name, reference, data)
    # Also store by digest
    storage.put_manifest(name, digest, data)
    # Update tag -> digest mapping
    storage.set_tag(name, reference, digest)

    base_url = str(request.base_url).rstrip("/")
    manifest_url = f"{base_url}/v2/{name}/manifests/{digest}"
    return Response(
        status_code=201,
        headers={
            "Location": manifest_url,
            "Docker-Content-Digest": digest,
        },
    )


# ---------------------------------------------------------------------------
# Admin / debug endpoints (not part of registry protocol)
# ---------------------------------------------------------------------------

@app.get("/_debug/blobs")
async def list_blobs() -> JSONResponse:
    """List all stored blobs (for debugging)."""
    if isinstance(storage, MemoryStorage):
        return JSONResponse({
            "blobs": list(storage.blobs.keys()),
            "manifests": list(storage.manifests.keys()),
            "tags": list(storage.tags.keys()),
        })
    return JSONResponse({"message": "FileStorage listing not implemented"})


@app.delete("/_debug/reset")
async def reset_storage() -> PlainTextResponse:
    """Clear all stored data (for test isolation)."""
    if isinstance(storage, MemoryStorage):
        storage.blobs.clear()
        storage.manifests.clear()
        storage.tags.clear()
    return PlainTextResponse("OK", status_code=200)


# ---------------------------------------------------------------------------
# CLI entry point
# ---------------------------------------------------------------------------

def main() -> None:
    parser = argparse.ArgumentParser(description="Pekobot Mock Registry Server")
    parser.add_argument("--host", default="127.0.0.1", help="Bind host")
    parser.add_argument("--port", type=int, default=0, help="Bind port (0 = random)")
    parser.add_argument("--storage-dir", type=Path, default=None, help="File-backed storage")
    args = parser.parse_args()

    global storage
    if args.storage_dir:
        storage = FileStorage(args.storage_dir)
        print(f"Using file-backed storage: {args.storage_dir}", file=sys.stderr)
    else:
        storage = MemoryStorage()
        print("Using in-memory storage", file=sys.stderr)

    import uvicorn
    config = uvicorn.Config(app, host=args.host, port=args.port, log_level="warning")
    server = uvicorn.Server(config)
    server.run()


if __name__ == "__main__":
    main()
