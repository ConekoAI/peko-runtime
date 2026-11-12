# ADR-103: `Fetch` reads public web pages; search, browsers and computer use stay outside the runtime

**Status:** Accepted
**Date:** 2026-11-12
**Touches:** `peko-rs/core/src/tools/builtin/fetch.rs`, `tools/installation.rs`, `docs/architecture/builtin-tools.md`.

---

## 1. Context

A peko had no way to read a web page except `curl` through `Bash`, and only owner runs get `Bash` (ADR-078). A run acting for anyone else could not read a link posted in its own conversation.

Web search, a browser, and computer use were considered with it. Each has mature MCP servers and command-line tools, and an owner run can install one on request: an MCP server is a `mcp/<id>/server.json` file the peko writes itself (ADR-075). Building them in would add a search vendor and its key, or a browser engine, to what the runtime maintains (ADR-062 and ADR-066 retired that kind of surface).

Reading one URL is different. It is small, needs no key, and the rules that make it safe (which addresses it may reach, what a run below owner may do with it, what the ledger records) can only be enforced inside the runtime.

## 2. Decisions

### D1. One built-in, `Fetch`: a GET of one URL, returned as text

`Fetch` takes `url`, optional `raw`, and optional `max_chars`. It sends one GET, follows up to 5 redirects, and returns the page as text: HTML is rendered to plain text with its links kept, text formats (plain text, JSON, XML and the like) come back unchanged, and a declared charset is decoded. It reads at most 2 MB, takes at most 30 seconds, and returns 40,000 characters unless asked for more (up to 200,000).

It has no request body, no caller-set headers, and refuses URLs that carry a user and password. It is the 26th built-in, installed with the runtime defaults.

### D2. It reaches public addresses only, at every trust level

Each hop's host must resolve to public addresses only. Loopback, private, link-local, carrier-NAT and reserved ranges are refused, as are IPv6 addresses that embed one of them, and a name is refused whole when any of its addresses is not public. The request is pinned to the address that was checked, and redirects are followed by hand so each hop is checked the same way.

The rule is the same for owner runs. An owner run that needs a local service has `Bash`. One rule keeps a page that injects instructions from steering any run at the daemon, another local service, or a cloud metadata address.

When a proxy is configured in the environment, the request goes through it, and the check covers the name's addresses as this computer resolves them.

### D3. The trust floor does not refuse it; the policy reviewer sees it

Every run gets `Fetch`. A URL is also a way to send data out, so the tool is not exempt from review: a call by a non-owner run goes to the policy reviewer against `POLICY.md`, as `Read` does (ADR-078 D7). Every call is in the tool ledger with its URL (ADR-079).

### D4. The peko is told what `Fetch` cannot read

A plain request reads only part of the web. The tool says so in three places:

- **Its description** states that it is not a browser and reads only part of the internet: no JavaScript pages, no sign-in pages, no sites that block automated requests, no PDFs or other files, no search.
- **Every failure** names its reason (a bot check, a refusal, a rate limit, a file that is not text) and tells the model not to retry the URL but to use another source or say the page could not be read.
- **A page with almost no text that carries scripts** comes back with a `note` that it was probably drawn by JavaScript.

`Fetch` identifies itself as `Peko/<version>` and does not imitate a browser to get past bot checks.

### D5. Returned text is data

Runtime tags in fetched text are defused (`neutralize_runtime_tags`), so a page cannot pass as the runtime's `<acting-for>` or `<runtime-context>` blocks, and the description tells the model that what comes back is information, never instructions.

### D6. Search, browsers and computer use are not built in

They stay MCP servers or command-line tools the owner asks the peko to install. A provider's own server-side search or fetch is a possible later addition where a model offers one; it is not part of this change.

## 3. Consequences

- A run acting for a contact can read a public link, subject to the owner's policy.
- Sites behind bot checks, pages drawn by JavaScript, and anything behind a sign-in still need a browser MCP server.
- Two new dependencies in `peko-rs/core`: `html2text` (and `html5ever` under it) and `encoding_rs` (already in the tree through `reqwest`).
- An untrusted page's text enters the conversation of whatever run fetched it. In an owner run that text sits beside `Bash`. D5 lowers the risk; it does not remove it, and it is the same risk `curl` through `Bash` already carried.
