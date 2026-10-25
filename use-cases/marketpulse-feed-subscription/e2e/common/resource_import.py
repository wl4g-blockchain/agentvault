"""Read the canonical AgentVault console-resource fixture without PyYAML.

Each YAML document is deliberately encoded as JSON (JSON is a YAML subset),
so the E2E runner can retain its standard-library-only dependency boundary.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit, urlunsplit


COLLECTION_BY_KIND = {
    "Client": "clients",
    "Wallet": "wallets",
    "Scenario": "scenarios",
    "Policy": "policies",
    "Offer": "offers",
    "Approver": "approvers",
}

_CANONICAL_MARKETPULSE_ORIGIN = "http://127.0.0.1:8081"
_MARKETPULSE_OFFER = "offer-realtime-news"


def _resource_documents(path: Path) -> list[dict[str, Any]]:
    documents: list[dict[str, Any]] = []
    bodies = [body.strip() for body in path.read_text(encoding="utf-8").split("\n---\n")]
    for index, body in enumerate(bodies, start=1):
        body = body.removeprefix("---\n").strip()
        if not body:
            continue
        document = json.loads(body)
        if set(document) != {"consoleVersion", "kind", "metadata", "data"}:
            raise ValueError(f"resource document {index} has an invalid envelope")
        if document["consoleVersion"] != "core.agentvault.io/v1":
            raise ValueError(f"resource document {index} has an unsupported consoleVersion")
        kind = document["kind"]
        if kind not in COLLECTION_BY_KIND:
            raise ValueError(f"resource document {index} has an unsupported kind: {kind}")
        metadata = document["metadata"]
        data = document["data"]
        if set(metadata) != {"name", "status"} or not isinstance(data, dict):
            raise ValueError(f"resource document {index} has invalid metadata or data")
        documents.append(document)
    if not documents:
        raise ValueError("resource fixture is empty")
    return documents


def _normalized_origin(origin: str) -> str:
    parsed = urlsplit(origin)
    if (
        parsed.scheme not in {"http", "https"}
        or not parsed.hostname
        or parsed.username is not None
        or parsed.password is not None
        or parsed.query
        or parsed.fragment
        or parsed.path not in {"", "/"}
    ):
        raise ValueError("MarketPulse origin must be an absolute HTTP(S) origin without credentials or a path")
    return urlunsplit((parsed.scheme, parsed.netloc, "", "", ""))


def render_agentvault_resources(path: Path, marketpulse_origin: str) -> str:
    """Render the canonical import fixture for one runtime origin.

    Environment projection is intentionally limited to the origin-bound Policy
    and Offer fields.  All authoritative identifiers and payment constraints
    remain byte-for-byte equivalent to the canonical fixture.
    """
    origin = _normalized_origin(marketpulse_origin)
    documents = _resource_documents(path)
    policy_count = 0
    offer_count = 0
    for document in documents:
        data = document["data"]
        if document["kind"] == "Policy":
            if data.get("allowed_origins") != [_CANONICAL_MARKETPULSE_ORIGIN]:
                raise ValueError("canonical Policy allowed_origins drifted from the expected E2E origin")
            data["allowed_origins"] = [origin]
            policy_count += 1
        elif document["kind"] == "Offer" and document["metadata"]["name"] == _MARKETPULSE_OFFER:
            if data.get("resource") != f"{_CANONICAL_MARKETPULSE_ORIGIN}/mcp":
                raise ValueError("canonical MarketPulse Offer resource drifted from the expected E2E URL")
            data["resource"] = f"{origin}/mcp"
            offer_count += 1
    if policy_count == 0 or offer_count != 1:
        raise ValueError("canonical fixture must contain Policies and exactly one MarketPulse Offer")
    return "\n".join(
        f"---\n{json.dumps(document, separators=(',', ':'), ensure_ascii=False)}"
        for document in documents
    ) + "\n"


def load_agentvault_resources(path: Path) -> dict[str, list[dict[str, Any]]]:
    """Load strict resource documents and expose their domain data by kind."""
    collections = {name: [] for name in COLLECTION_BY_KIND.values()}
    for document in _resource_documents(path):
        kind = document["kind"]
        collection = COLLECTION_BY_KIND[kind]
        metadata = document["metadata"]
        data = document["data"]
        resource = dict(data)
        if kind != "Approver":
            resource["id"] = metadata["name"]
        resource["enabled"] = metadata["status"] == "active"
        collections[collection].append(resource)
    return collections
