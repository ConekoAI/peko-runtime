#!/usr/bin/env python3
"""Pekobot Mock Registry Server

A lightweight FastAPI-based mock registry implementing an OCI-inspired
protocol for .agent package push/pull. Used for integration testing.

Endpoints:
    GET    /v2/                          → 200 (capability check)
    GET    /v2/_catalog                  → list repositories
    GET    /v2/{name}/tags/list          → list tags
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
    python main.py --auth-token my-secret-token
"""

from __future__ import annotations

import argparse
import hashlib
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
# Configuration globals (set at startup)
# ---------------------------------------------------------------------------
AUTH_TOKEN: str | None = None

# ---------------------------------------------------------------------------
# Storage backends
# ---------------------------------------------------------------------------

class MemoryStorage:
    """In-memory blob and manifest storage."""

    def __init__(self) -> None:
        self.blobs: dict[str, bytes] = {}          # digest -> bytes
        self.manifests: dict[str, str] = {}        # "repo:ref" -> manifest_json
        self.manifest_media_types: dict[str, str] = {}  # "repo:ref" -> media_type
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

    def get_manifest_media_type(self, repo: str, ref: str) -> str | None:
        key = f"{repo}:{ref}"
        return self.manifest_media_types.get(key)

    def put_manifest(self, repo: str, ref: str, data: str, media_type: str | None = None) -> None:
        key = f"{repo}:{ref}"
        self.manifests[key] = data
        if media_type is not None:
            self.manifest_media_types[key] = media_type
        # Also store by digest if ref looks like a digest
        if ref.startswith("sha256:"):
            self.manifests[f"{repo}:{ref}"] = data
            if media_type is not None:
                self.manifest_media_types[f"{repo}:{ref}"] = media_type

    def get_tag_digest(self, repo: str, tag: str) -> str | None:
        return self.tags.get(f"{repo}:{tag}")

    def set_tag(self, repo: str, tag: str, digest: str) -> None:
        self.tags[f"{repo}:{tag}"] = digest

    def list_repositories(self) -> list[str]:
        repos = set()
        for key in self.manifests:
            # Keys are "repo:ref" where ref can be a tag (e.g., "v1.0") or digest ("sha256:...")
            # For tag refs: "ns/name:v1.0" → repo = "ns/name"
            # For digest refs: "ns/name:sha256:abc" → repo = "ns/name"
            # We split on ":" and look for "sha256:" to know where the ref starts
            if ":sha256:" in key:
                repo = key.rsplit(":sha256:", 1)[0]
            else:
                repo = key.rsplit(":", 1)[0]
            repos.add(repo)
        return sorted(repos)

    def list_tags(self, repo: str) -> list[str]:
        tags = []
        for key, digest in self.tags.items():
            k_repo, k_tag = key.rsplit(":", 1)
            if k_repo == repo:
                tags.append(k_tag)
        return sorted(tags)

    def clear(self) -> None:
        self.blobs.clear()
        self.manifests.clear()
        self.manifest_media_types.clear()
        self.tags.clear()


class FileStorage:
    """File-backed blob and manifest storage."""

    def __init__(self, base_dir: Path) -> None:
        self.base_dir = base_dir
        self.blobs_dir = base_dir / "blobs"
        self.manifests_dir = base_dir / "manifests"
        self.tags_dir = base_dir / "tags"
        self.media_types_dir = base_dir / "media_types"
        for d in (self.blobs_dir, self.manifests_dir, self.tags_dir, self.media_types_dir):
            d.mkdir(parents=True, exist_ok=True)

    def _blob_path(self, digest: str) -> Path:
        hex_part = digest.removeprefix("sha256:")
        return self.blobs_dir / f"sha256-{hex_part}.bin"

    def _manifest_path(self, repo: str, ref: str) -> Path:
        safe_repo = repo.replace("/", "_")
        return self.manifests_dir / f"{safe_repo}_{ref}.json"

    def _media_type_path(self, repo: str, ref: str) -> Path:
        safe_repo = repo.replace("/", "_")
        return self.media_types_dir / f"{safe_repo}_{ref}.txt"

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

    def get_manifest_media_type(self, repo: str, ref: str) -> str | None:
        path = self._media_type_path(repo, ref)
        if not path.exists():
            return None
        return path.read_text().strip()

    def put_manifest(self, repo: str, ref: str, data: str, media_type: str | None = None) -> None:
        path = self._manifest_path(repo, ref)
        path.write_text(data)
        if media_type is not None:
            self._media_type_path(repo, ref).write_text(media_type)
        if ref.startswith("sha256:"):
            self._manifest_path(repo, ref).write_text(data)
            if media_type is not None:
                self._media_type_path(repo, ref).write_text(media_type)

    def get_tag_digest(self, repo: str, tag: str) -> str | None:
        path = self._tag_path(repo, tag)
        if not path.exists():
            return None
        return path.read_text().strip()

    def set_tag(self, repo: str, tag: str, digest: str) -> None:
        path = self._tag_path(repo, tag)
        path.write_text(digest)

    def list_repositories(self) -> list[str]:
        repos = set()
        for p in self.manifests_dir.glob("*.json"):
            stem = p.stem  # safe_repo_ref
            # Remove the last _ref part; safe_repo may contain underscores
            # We know the original repo used / replaced with _
            # Find the rightmost underscore that separates repo from ref
            # A ref is either a tag or sha256:... (with colon replaced? No, we keep colon in filename)
            # stem format: safe_repo_ref where ref can contain colons
            # Split from the right on '_' but need to distinguish repo '_' from separator '_'.
            # Since repo parts are separated by '_' (from '/'), we can't perfectly reverse.
            # Instead, read the manifest content and parse repo from key? No key inside.
            # Heuristic: try to match known tag files to confirm repo names.
            pass
        # Better approach: iterate tags directory to collect repo names
        for p in self.tags_dir.glob("*.txt"):
            stem = p.stem
            # tag files: safe_repo_tag.txt
            # We need to know the tag to split. Tags don't contain underscores usually,
            # but to be safe let's read the tag from filename by matching against known tags.
            # Simpler: iterate all manifests and derive repo from associated tag files.
            pass
        # Alternative: scan manifests and for each manifest find its repo by trying all tags.
        # Simpler and robust: use tags directory since every pushed manifest has a tag mapping.
        repos = set()
        for p in self.tags_dir.glob("*.txt"):
            stem = p.stem
            # The tag is the part after the last underscore that corresponds to a known tag.
            # Since we don't know tags a priori, let's use a different strategy:
            # Store a repo index file.
            pass
        # Use index file for simplicity
        index_path = self.base_dir / "repositories.json"
        if index_path.exists():
            return sorted(json.loads(index_path.read_text()))
        return []

    def list_tags(self, repo: str) -> list[str]:
        safe_repo = repo.replace("/", "_")
        tags = []
        for p in self.tags_dir.glob(f"{safe_repo}_*.txt"):
            tag = p.stem[len(safe_repo) + 1:]
            tags.append(tag)
        return sorted(tags)

    def _update_repo_index(self, repo: str) -> None:
        index_path = self.base_dir / "repositories.json"
        repos = set()
        if index_path.exists():
            repos = set(json.loads(index_path.read_text()))
        repos.add(repo)
        index_path.write_text(json.dumps(sorted(repos)))

    def put_manifest(self, repo: str, ref: str, data: str, media_type: str | None = None) -> None:
        path = self._manifest_path(repo, ref)
        path.write_text(data)
        if media_type is not None:
            self._media_type_path(repo, ref).write_text(media_type)
        if ref.startswith("sha256:"):
            digest_path = self._manifest_path(repo, ref)
            digest_path.write_text(data)
            if media_type is not None:
                self._media_type_path(repo, ref).write_text(media_type)
        self._update_repo_index(repo)

    def set_tag(self, repo: str, tag: str, digest: str) -> None:
        path = self._tag_path(repo, tag)
        path.write_text(digest)
        self._update_repo_index(repo)

    def clear(self) -> None:
        for d in (self.blobs_dir, self.manifests_dir, self.tags_dir, self.media_types_dir):
            for f in d.iterdir():
                f.unlink()
        index_path = self.base_dir / "repositories.json"
        if index_path.exists():
            index_path.unlink()


# Global storage — set at startup
storage: MemoryStorage | FileStorage = MemoryStorage()

# ---------------------------------------------------------------------------
# OCI error helpers
# ---------------------------------------------------------------------------

def oci_error(code: str, message: str, status_code: int) -> JSONResponse:
    return JSONResponse(
        status_code=status_code,
        content={"errors": [{"code": code, "message": message}]},
    )


# ---------------------------------------------------------------------------
# Auth helper
# ---------------------------------------------------------------------------

def require_auth(request: Request) -> JSONResponse | None:
    """Return an error response if auth is required but missing/invalid."""
    if AUTH_TOKEN is None:
        return None
    auth_header = request.headers.get("Authorization", "")
    expected = f"Bearer {AUTH_TOKEN}"
    if auth_header != expected:
        return oci_error(
            "UNAUTHORIZED",
            "Authentication is required",
            401,
        )
    return None


# ---------------------------------------------------------------------------
# Namespace validation
# ---------------------------------------------------------------------------

def validate_name(name: str) -> JSONResponse | None:
    """Return an error response if the repository name is invalid."""
    if "/" not in name:
        return oci_error(
            "NAME_UNKNOWN",
            f"Repository name must contain a namespace separator '/': {name}",
            404,
        )
    return None


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _normalize_digest(digest: str) -> str:
    """Ensure digest has sha256: prefix."""
    if ":" not in digest:
        return f"sha256:{digest}"
    return digest


# Accepted manifest media types
MANIFEST_MEDIA_TYPES = {
    "application/vnd.peko.manifest.v1+json",
    "application/vnd.oci.image.manifest.v1+json",
}

DEFAULT_MANIFEST_MEDIA_TYPE = "application/vnd.oci.image.manifest.v1+json"

# ---------------------------------------------------------------------------
# Routes
# ---------------------------------------------------------------------------

@app.get("/v2/")
async def registry_check() -> Response:
    """Registry capability check."""
    return Response(status_code=200)


@app.get("/v2/_catalog")
async def catalog(n: int | None = None, last: str | None = None) -> JSONResponse:
    """List all repositories."""
    repos = storage.list_repositories()
    if last is not None:
        repos = [r for r in repos if r > last]
    if n is not None:
        repos = repos[:n]
    return JSONResponse({"repositories": repos})


@app.get("/v2/{name:path}/tags/list")
async def list_tags(name: str, n: int | None = None, last: str | None = None) -> JSONResponse:
    """List tags for a repository."""
    err = validate_name(name)
    if err:
        return err
    tags = storage.list_tags(name)
    if last is not None:
        tags = [t for t in tags if t > last]
    if n is not None:
        tags = tags[:n]
    return JSONResponse({"name": name, "tags": tags})


@app.head("/v2/{name:path}/blobs/{digest}")
async def check_blob(name: str, digest: str) -> Response:
    """Check if a blob (layer) exists."""
    err = validate_name(name)
    if err:
        return Response(status_code=404)
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
    err = validate_name(name)
    if err:
        return err
    digest = _normalize_digest(digest)
    data = storage.get_blob(digest)
    if data is None:
        return oci_error(
            "BLOB_UNKNOWN",
            f"Blob not found: {digest}",
            404,
        )
    return Response(
        content=data,
        media_type="application/octet-stream",
        headers={"Docker-Content-Digest": digest},
    )


@app.post("/v2/{name:path}/blobs/uploads/")
async def initiate_upload(name: str, request: Request) -> Response:
    """Initiate a blob upload — returns upload URL."""
    err = validate_name(name)
    if err:
        return err
    auth_err = require_auth(request)
    if auth_err:
        return auth_err
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
    """Complete a blob upload.

    Validates that the client sends the ?digest=sha256:... query parameter
    and that the provided digest matches the body content.
    """
    err = validate_name(name)
    if err:
        return err
    auth_err = require_auth(request)
    if auth_err:
        return auth_err

    body = await request.body()

    # Require digest query parameter (OCI spec compliance)
    if digest is None or not digest:
        return oci_error(
            "DIGEST_INVALID",
            "Missing required digest query parameter",
            400,
        )

    digest = _normalize_digest(digest)
    hex_part = digest.removeprefix("sha256:")

    # Validate digest matches body content
    computed = hashlib.sha256(body).hexdigest()
    if computed != hex_part:
        return oci_error(
            "DIGEST_INVALID",
            f"Digest mismatch: expected sha256:{computed}, got {digest}",
            400,
        )

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
    err = validate_name(name)
    if err:
        return err

    # Try direct lookup first
    data = storage.get_manifest(name, reference)
    media_type = storage.get_manifest_media_type(name, reference)
    if data is None:
        # Try resolving tag to digest, then lookup by digest
        tag_digest = storage.get_tag_digest(name, reference)
        if tag_digest:
            data = storage.get_manifest(name, tag_digest)
            media_type = storage.get_manifest_media_type(name, tag_digest)

    if data is None:
        return oci_error(
            "MANIFEST_UNKNOWN",
            f"Manifest not found: {name}:{reference}",
            404,
        )

    if media_type is None:
        media_type = DEFAULT_MANIFEST_MEDIA_TYPE

    return Response(
        content=data,
        media_type=media_type,
    )


@app.put("/v2/{name:path}/manifests/{reference}")
async def put_manifest(name: str, reference: str, request: Request) -> Response:
    """Push a manifest."""
    err = validate_name(name)
    if err:
        return err
    auth_err = require_auth(request)
    if auth_err:
        return auth_err

    content_type = request.headers.get("Content-Type", "")
    if content_type not in MANIFEST_MEDIA_TYPES:
        return oci_error(
            "MANIFEST_INVALID",
            f"Unsupported Content-Type: {content_type}. Supported: {', '.join(sorted(MANIFEST_MEDIA_TYPES))}",
            400,
        )

    body = await request.body()
    data = body.decode("utf-8")

    # Compute manifest digest
    computed = hashlib.sha256(body).hexdigest()
    digest = f"sha256:{computed}"

    # Store by tag
    storage.put_manifest(name, reference, data, media_type=content_type)
    # Also store by digest
    storage.put_manifest(name, digest, data, media_type=content_type)
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
    storage.clear()
    return PlainTextResponse("OK", status_code=200)


# ---------------------------------------------------------------------------
# CLI entry point
# ---------------------------------------------------------------------------

def main() -> None:
    parser = argparse.ArgumentParser(description="Pekobot Mock Registry Server")
    parser.add_argument("--host", default="127.0.0.1", help="Bind host")
    parser.add_argument("--port", type=int, default=0, help="Bind port (0 = random)")
    parser.add_argument("--storage-dir", type=Path, default=None, help="File-backed storage")
    parser.add_argument("--auth-token", type=str, default=None, help="Bearer token for mutating operations")
    args = parser.parse_args()

    global storage, AUTH_TOKEN
    AUTH_TOKEN = args.auth_token
    if args.storage_dir:
        storage = FileStorage(args.storage_dir)
        print(f"Using file-backed storage: {args.storage_dir}", file=sys.stderr)
    else:
        storage = MemoryStorage()
        print("Using in-memory storage", file=sys.stderr)

    import uvicorn
    import socket

    # If port is 0, bind a socket ourselves to get an ephemeral port,
    # then pass that port to uvicorn.
    actual_port = args.port
    if args.port == 0:
        sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        sock.bind((args.host, 0))
        actual_port = sock.getsockname()[1]
        sock.close()
        print(f"PORT={actual_port}", flush=True)

    config = uvicorn.Config(app, host=args.host, port=actual_port, log_level="warning")
    server = uvicorn.Server(config)
    server.run()


if __name__ == "__main__":
    main()
