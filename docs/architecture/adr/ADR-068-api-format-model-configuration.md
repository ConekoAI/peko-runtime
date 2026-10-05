# ADR-068: API-format model configuration

- Status: Accepted
- Date: 2026-10-05

## Context

Maintaining vendor presets, model ids, capabilities, prices, and endpoint
aliases duplicates upstream catalogs. Peko already selects its adapter by API
format, and every configured model already includes endpoint settings.

## Decision

Maintain adapters for OpenAI Chat Completions, OpenAI Responses, and Anthropic
Messages. Configure models through explicit base URL, wire model id, API format,
and a vault credential. A configured name is optional and defaults to the wire id.
Remove the embedded vendor/model gallery and vendor convenience constructors.

Limits, headers, capabilities/pricing, notes, and adapter compatibility hints
are optional user-authored metadata. An absent spec means unknown, not text-only
or unsupported; the existing engine gate stays inactive until a spec is supplied.
Protocol compatibility parsing and reasoning-history handling remain adapter
responsibilities. Existing compatibility annotations remain supported; this change
does not redesign wire protocols or add arbitrary request transformation machinery.

CLI add and daemon IPC use the same explicit settings and validate metadata before
credential writes. Keyless endpoints are explicit. Copied CLI commands include
endpoint settings and optional metadata with shell quoting.

## Compatibility

Saved entries retain copied endpoints, credentials, limits, spec, compat, and legacy
`template_id`; they do not need a preset lookup. New entries have no template origin.
`--custom` and IPC `custom` remain accepted but unnecessary. Template add requests
are rejected with instructions to provide explicit settings. The old
`ModelTemplates` wire endpoint returns an empty list for desktop custom-model flows.
Desktop clients should replace their gallery with the generic endpoint form.

The opt-in environment bootstrap derives `<ID>_API_KEY` mechanically (or the
legacy template id). Vendor aliases such as moonshot-to-KIMI are retired;
production keys remain in the vault.

## Consequences

New compatible endpoints and models need no runtime release. Users author metadata
that the API format cannot establish. Adding a new adapter is reserved for a
fundamentally different wire format. `model test` retains its existing endpoint/auth
probe semantics; it does not discover or certify model capabilities.
