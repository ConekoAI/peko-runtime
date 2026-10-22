# Persistent bots, round 2: Muse, Grok Bot, Dots and Duet vs. Peko — gaps and roadmap

**Date:** 2026-10-22 · **Status:** research note (not an ADR) · **Follows:**
[round 1](2026-10-persistent-bot-patterns.md) (2026-10-14, Muse and Grok Bot
harness internals)

Round 1 studied *how* Muse and Grok Bot keep a bot alive: memory, focus and
heartbeat. Most of its patterns have since shipped (ADR-078 to ADR-086).
Round 2 widens the field to four products: OpenAI **Dots** (2026-09-29) and
Anthropic **Claude Duet** (2026-10-21) are new, and **Muse** and **Grok Bot**
have changed. It also asks a different question: what does a user of these
products get that a Peko user does not? That is a product question more than
a harness question. The answer sets the roadmap.

## 0. Source quality

- **[vendor]** Launch posts, help centers and changelogs: Anthropic's Duet
  announcement and getting-started guide; OpenAI's dots post, help center and
  safety FAQ (read via a docs-only write-up, because openai.com sits behind a
  bot challenge); the Grok Bot changelog (v0.71.2, 2026-10-14); Meta's Muse
  for Small Business and Muse Wonder posts.
- **[3p]** Press and reviewers: VentureBeat, Engadget, MacRumors, Every's
  nine-agent comparison, Flavio Copes's Duet deep dive, AIMultiple. None of
  these is a hands-on benchmark. **No public source runs the same tasks
  through all four.**
- **[code]** Peko claims are verified on `master` @ `97244940` unless marked.
- **[spec]** My own inference.

---

## 1. What changed since round 1

| Product | New since 2026-10-14 |
|---|---|
| **Claude Duet** (new) | Research preview, US Pro/Max, waitlist. One agent on Opus 5.5 (you cannot pick a different model). Runs in a cloud VM with a browser **[vendor]**. |
| **OpenAI Dots** (new) | One "primary dot" per user, on GPT-6 Astra. It has its own cloud computer and browser, can optionally access your local machine, and reaches 4,000+ apps. Reachable in Slack and Teams. "Specialist dots" with their own org identity are in pilot. ChatGPT Space adds shared pages **[vendor]**. |
| **Grok Bot** | Bots now have their own email inbox (`@mail.grokbot.com`; a `+tag` address routes mail to one routine). Routines trigger on schedule, Slack, GitHub, Linear, Sentry, PagerDuty, email or webhook. Team Bots live in Slack. Admins can switch off bot-to-bot messaging. Secure Forms. Up to 200 skills from plugins. A phone call wakes the bot's sleeping computer **[vendor]**. |
| **Muse** | Muse for Small Business: a goal-driven agent with Meta ads and Instagram analytics plus 15 outside connectors. Muse Wonder models. Coming soon: EU and WhatsApp, an iPad app, glasses, a video avatar, and computer use on Mac **[vendor]**. |

The four products now **converge on one shape**. Peko's gaps are measured
against that shape.

---

## 2. The converged product shape (the bar for "on par")

Every item below appears in at least three of the four products.

### 2.1 It lives somewhere that is always on

Every product runs on a vendor-hosted VM or "computer" with a browser. "Keeps
working when you close your laptop" is the headline promise of all four.
Dots can also reach your own machine (opt-in, off by default). Grok Bot
sleeps its computer and wakes it on a message or call **[vendor]**.

### 2.2 It reaches you wherever you are

- **Mobile app with push.** All four have one.
- **Other channels:**
  - Dots: Slack and Teams, with SMS in limited beta.
  - Grok Bot: Slack (Team Bots) and email.
  - Muse: WhatsApp is coming.
  - Duet: in-app only for now.
- **Voice.** All four have in-app voice. Grok Bot and Dots accept phone calls
  in. Nobody calls out yet.
- **Configurable interruptions.** Duet "messages you only when something needs
  attention, and you can set what you want to hear about and how often."
  Taskade's review names *noise* as the first way proactive agents fail
  **[vendor] [3p]**.

### 2.3 It looks before you ask, but background looking is read-only

- **Duet:** a morning brief, then a check of your connected apps "every few
  hours." Anthropic chose that interval to protect plan limits.
- **Dots:** "proactive research" whose tools *cannot* send messages, edit
  content or drive the browser. Custom Rules cannot lift this restriction.
- **Grok Bot:** event-triggered routines.
- **Muse:** proactive check-ins against goals.

**[vendor]** for all four.

### 2.4 Acting on your behalf is tiered, and the tiers apply to you

This is the strongest convergence.

- **Dots, Custom Rules.** Four levels: *act without asking*, *act if
  pre-approved* (you asked for it explicitly), *ask first*, *hand off to you*.
  An **auto-review** checks every consequential action against your
  instructions, your rules and the safety requirements. Approving one message
  does not grant ongoing permission. Changing a password or moving money is
  always handed to the human.
- **Duet.** It always asks before it sends, pays, signs or deletes. Trading
  securities and moving money between accounts are refused even with approval.
  An auto-mode classifier screens each action.
- **Grok Bot.** Auto Review with *Ask first* and *Allow automatically* rules;
  when both match, *Ask first* wins.
- **Muse.** "Nothing publishes, sends, or spends without your approval."

**[vendor]** for all four.

### 2.5 Credentials never reach the model

- Duet follows the Claude Managed Agents design: credentials stay outside the
  sandbox and tools use them through controlled infrastructure.
- Dots and Grok Bot use **Secure Forms**, which send fields straight to the
  browser and keep them out of the model.
- Purchases go through scoped single-use cards: Stripe Link and Shop Pay in
  Duet.
- Duet also runs an egress classifier. It refuses to share card or
  government-ID numbers, and shares health and finance details only on
  explicit instruction.

**[vendor]**

### 2.6 You can see what it is doing

- **Dots:** an Activity View (In progress / Scheduled / Completed) where you
  can step in mid-task. You can also open its cloud computer and take over.
- **Grok Bot:** working-status avatars, plus a takeover of the bot's computer.
- **Duet:** *Tasks* and *Make* tabs, and **only you can mark a task
  complete**. It cannot show you its browser yet; reviewers call this its
  biggest weakness.

**[vendor] [3p]**

### 2.7 Thousands of connectors, managed for you

Duet has 6,000+ connectors, Dots 4,000+, and Grok Bot and Muse curated sets.
In every case OAuth is a click, and the vendor maintains the integration
**[vendor]**.

### 2.8 Memory is ambient, but often opaque

- **Dots:** memory cannot be viewed, edited or deleted item by item; resetting
  the dot deletes everything.
- **All four:** disconnecting an app does not purge what was learned from it.
- **Duet:** memory is shared with your other Claude chats, and channels share
  memory with one another.

**[vendor] [3p]**

### 2.9 Several agents, but coordination is shallow

- **Grok Bot:** up to ~50 named Bots on one shared computer. Their screens are
  "not security boundaries."
- **Dots:** one primary dot; specialist dots are in pilot.
- **Duet and Muse:** one agent each.

**[vendor]**

---

## 3. Where Peko stands

### 3.1 Where Peko is ahead (keep and sell these)

| Axis | The four | Peko |
|---|---|---|
| **Ownership and portability** | Vendor account; nothing is portable | Principal with a DID, portable `.peko` snapshots, contacts and owner carried across runtimes (ADR-081) |
| **Memory you can read and edit** | Dots: no per-item view or delete. Others: partial | Plain files in `kb/`. Hot lines carry `(source: …)`, and a reviewer model checks them against their sources (ADR-080, ADR-085). Retraction is an ordinary edit. |
| **Model choice** | Locked to one vendor model | Any model by wire format (ADR-068), with a per-job model per cron job |
| **Real compartments between audiences** | Grok: screens are not boundaries. Duet: channels share memory | Each conversation is its own compartment, with a trust floor and acting-for stamps (ADR-078) |
| **Multiple agents as peers** | One agent, or a flat list of bots | Many principals, each with its own identity, contacts, group channels and wake policy (ADR-049, ADR-082). Signed cross-runtime delivery (ADR-058, ADR-072). |
| **Audit** | Activity view (vendor-held) | A per-principal ledger with hash chaining (ADR-079), readable by the agent through `ToolLog` |
| **Rules you can read** | Rules hidden behind a settings UI | `POLICY.md`, plain language, owned by the owner |
| **Background cost discipline** | Duet's background checks spend plan limits | Quiet ticks call no model when nothing waits (ADR-083); check-ins via `checkAt` (ADR-084); peer nudges (ADR-086) |

### 3.2 Gaps (continuing round 1's numbering)

Severity is measured against the bar in §2.

| # | Sev | Gap | Evidence |
|---|---|---|---|
| G13 | **Critical** | **There is no way to reach the owner.** A proactive note goes into the peer's session JSONL and "never triggers a turn"; the human sees it only when they next open a client. There is no push, mobile app, email, Slack, Telegram or WhatsApp. The chat-gateway adapters were retired in sprint 9. Hub webchat serves *public callers* of exposed pekos, not the owner. Peko-desktop has no OS notifications and was last updated before ADR-072. | `core/src/principal/messenger.rs:1-15`; AGENTS.md §5; pekohub README; peko-desktop `git log` |
| G14 | **Critical** | **The owner's own runs are ungated, including autonomous ones.** The policy judge returns early for any owner stamp. Scheduled keepalive and memory runs act as the owner, so a background tick can call an MCP "send", "pay" or "delete" tool with no approval. This is the market's rule turned upside down: everyone else lets the background *read*, and gates the owner's *side effects*. | `core/src/security/judge.rs:136`; ADR-078 D7 |
| G15 | **High** | **It does not stay on.** Peko is a local daemon on the user's machine: no container image, no hosted mode, no documented "run it on a VPS" path. If the laptop sleeps, the bot sleeps. | no Dockerfile at the root; MVP_GATE gate 1 (no release yet) |
| G16 | **High** | **There is no shell sandbox.** `Bash` runs as the OS user, so the trust floor and ledger are "tamper-evident, not tamper-proof." The four products put the agent in a VM. Peko gives untrusted runs no Bash at all; owner and trusted runs share the user's account. | ADR-079; `security/floor.rs` `UNTRUSTED_DENIED_TOOLS` |
| G17 | **High** | **No browser and no web tools.** No browser automation, no fetch or search builtin; web access is `curl` through Bash, or an MCP server the user has to find and configure. There is no takeover view and no Secure Form equivalent. | `docs/architecture/builtin-tools.md` (23 tools, no web) |
| G18 | **High** | **No event triggers.** Cron is time-based, idle-based or attention-gated only. `MessageSource::Webhook` exists as an enum variant with no ingress behind it. Nothing wakes the bot on new mail, a GitHub event or a webhook. | `session/src/events.rs:58`; Cron `create` schema |
| G19 | **High** | **Connecting an app takes engineering work.** MCP is principal-owned with OAuth (ADR-075), which is a good primitive. But there is no curated catalog and no one-click connect: users write `mcp/<id>/server.json` by hand. | builtin-tools.md §MCP |
| G20 | **Med** | **Credentials reach the model through Bash.** Provider keys are vaulted and MCP OAuth tokens are held by the runtime. Anything else (an API token a skill needs, a site login) has to pass through the conversation or a file, and redaction happens only *after the fact*, in the ledger (ADR-079 D3). | ADR-079 D3 |
| G21 | **Med** | **Nothing shows the owner what the bot is doing.** The pieces exist (ledger, plans, cron history, Async and Task state) but are reachable only through CLI and `ToolLog`. Nothing renders an In progress / Scheduled / Completed view, nothing pauses or redirects a background task, and nothing stops the agent from marking its own work done. | `peko audit ledger`; Plan / Cron / Async tools |
| G22 | **Med** | **No standard brief or daily rhythm.** Genesis seeds `keepalive` and `memory` only. A morning brief, the feature every product leads its marketing with, is left to the user to configure. | `core/src/principal/genesis.rs:63-81` |
| G23 | **Med** | **No egress screen.** Nothing checks an outbound post or tool argument for card numbers, government IDs or secrets. `review_replies` judges only replies to non-owners, and only against `POLICY.md`. | `security/policy.rs` |
| G24 | **Med** | **Retrieval is grep only.** There is no ranked search over `kb/` plus transcripts, and no `memory_explain` that walks a line back to its source. Round 1 deferred this; Duet's search across past chats raises the bar. | MVP_GATE "not gating" list |
| G25 | **Low-Med** | **The bot has no identity of its own on the wider internet.** No email address, phone number or Slack identity, unlike Grok's `@mail.grokbot.com`, Dots' specialist identities and Instinct's agent email. Peko's DID and Hub ID are strong *internally* but invisible to the services people already use. | ADR-081 |
| G26 | **Low-Med** | **No onboarding import.** Duet and Dots start from the user's existing assistant memory. Peko starts empty: there is no importer for a ChatGPT or Claude memory export. | genesis |
| G27 | **Low** | **No voice.** There is no speech in or out on any surface. | `grep` for speech, TTS or Whisper: none |
| G28 | **Low** | **No payments.** There is no scoped-card or wallet integration. This is correctly deferred; it is the highest-risk surface in the category. | — |

### 3.3 Cost and quality signal

- `peko-benchmark` responsibility rerun 3 (n=10, `13dced92`): Peko met
  **21/30 obligations, the same as OpenClaw, at about 1.9× OpenClaw's cost
  per run** (MVP_GATE §5).
- Peko cannot match the vendors' tuned frontier models; Muse Wonder claims
  focus is trained into the model. Its parity strategy therefore has to be
  **harness efficiency on whatever model the user brings**. Cost per useful
  action is a first-class metric, not an afterthought.

---

## 4. Positioning

Peko should not try to be "Duet, but self-hosted, with fewer connectors." The
OSS peers it competes with directly are OpenClaw and Hermes Agent: both
self-hostable, both bring-your-own-model, both reachable on WhatsApp, Slack and
Telegram. Peko's defensible position:

> **The persistent agent you own.** It runs where you choose, on the model
> you choose. Its memory is readable files with sources, its rules are a
> file you wrote, every action it takes is in a ledger you can verify, and
> you can move it to another machine with its identity intact.

Each of those claims answers a published criticism of the closed products:

- TIME's dossier story on Muse.
- Dots' memory that cannot be viewed or deleted item by item.
- Grok Bot's shared computer, which is no security boundary.
- Duet's browser that you cannot watch.

What is missing is **not** the persistence core. Round 1 closed most of
that. What is missing is the **product envelope**: reach, being always on,
hands, and a safe way to act. The roadmap is ordered by that.

---

## 5. Roadmap

Sizes: S ≈ days, M ≈ 1–2 weeks, L ≈ several weeks. Each phase ends in
something a user can feel. Phases are ordered; items within a phase can run
in parallel.

### R0 — Ship the MVP (now; gates everything below)

MVP_GATE stands at 12/26. Its own warning applies: *shipping non-gating work
while gate items stand still is the pattern to avoid.* Cut the release, verify
a clean-machine install, run the field test and fold `[Unreleased]`.

**One addition to gate 6:** document **G14** (owner and scheduled runs are
ungated) as an accepted gap in the user's guide. Better still, ship R1.1's
minimal version first, because it is a safety property and not a feature.

### R1 — Act safely: tiered actions for the owner's own runs (G14, G23, G20)

This is the market's strongest convergence and Peko already has most of the
machinery: the judge, the `approve <id>` / `deny <id>` flow, `POLICY.md` and
the ledger.

1. **Action tiers in `POLICY.md`** (M). Add an owner section with four
   levels, per tool or MCP method pattern:
   - `allow`
   - `allow-if-asked`: the owner's message in this run named the action
   - `ask`
   - `never`: hand it to the human

   Ship conservative defaults. Outbound messages to anyone but the owner,
   MCP methods whose names suggest `send|post|create|delete|pay|transfer`,
   and `Bash` commands with a network verb all default to `ask`.
2. **Read-only background lane** (M). Runs started by cron or the attention
   keepalive, with no human message in the run, carry a `background` mark on
   their stamp. In that lane every side-effecting call follows its tier with
   `allow` downgraded to `ask`, and `ask` *queues an approval card* instead of
   blocking the run. This is the Dots rule, and it fits ADR-072's "only humans
   and cron start runs."
3. **Hard prohibitions** (S). A fixed floor list that no policy can lift:
   moving money, changing credentials, and deleting outside the workspace.
   This follows the existing ADR-078 floor pattern.
4. **Egress screen** (S–M). A deterministic detector for card numbers,
   government IDs and known vault secrets on outbound `ChannelSend` and
   MCP arguments. It refuses, or asks, with an optional judge for the
   ambiguous cases. Reuse the ADR-079 redactor's secret set.
5. **Secret references** (M). Tool arguments may carry `{{secret:<label>}}`,
   resolved in the dispatcher *after* the ledger write and *never* shown to
   the model. Add `peko vault secret set`. This closes G20 without a
   browser.

New ADR (≈ ADR-087): *Owner-run action tiers and the background lane.*

### R2 — Reach: the bot can find you (G13, G22, G21-lite)

1. **Owner notification channel** (M). Add an `owner-reach` binding beside
   ADR-082 bindings. When the agent posts to the owner (`user:local`), the
   runtime also delivers through every configured *reach adapter*. A post
   marked `needs_decision` (approval cards from R1) always goes out; others
   follow a quiet-hours and digest policy, which answers Taskade's "noise"
   failure.
2. **First reach adapters** (M each). Pick channels with a cheap bot API and
   no business verification:
   - **Telegram** first, both directions.
   - **Email** (SMTP out, IMAP in, which also feeds R4's triggers).
   - **Web push** through pekohub, for owners who have linked the runtime
     (ADR-081 already links the runtime's local user to a Hub account). The
     hub stays a relay; payloads stay signed (ADR-058).
   - **Slack** after that. WhatsApp last (business verification).

   Adapters are channel transports, not a revived gateway framework: an
   inbound message funnels through `PeerChildTurns::ensure_child_ingress` like
   every other ingress path.
3. **Approve from anywhere** (S, depends on 1 and 2). An approval card
   rendered on Telegram, email or push, with signed one-tap approve and deny
   routed to the existing `approve <id>` handler.
4. **Seeded morning brief** (S). A third genesis job, `brief`: a cron at 07:30
   local time, `when: attention`, using a brief template. It covers what
   changed since yesterday, decisions waiting on the owner, today's plan
   check-ins, and every item linked to its source. The owner can retune it
   like `keepalive` (#477 pattern).
5. **Desktop catch-up** (M, peko-desktop). Bring the desktop app up to
   ADR-072 to ADR-086, add OS notifications and the approval card, and render
   a first Activity view (R4.3).

### R3 — Always on: a home that never sleeps (G15, G16)

1. **Container image and headless deploy** (M). A `Dockerfile`, a
   `docker-compose` example and a documented VPS / Fly / Lightsail path.
   Vault unlock uses `PEKO_UNLOCK_METHOD=passphrase` or a mounted key file.
   The owner reaches it through R2 adapters and the hub tunnel. This turns
   "my laptop must be on" into "a $5 box is on."
2. **Sandboxed shell** (L). Run `Bash` (and later the browser) in a
   per-principal sandbox: a container when Docker is available, otherwise
   `bwrap` on Linux and `sandbox-exec` on macOS. The workspace is mounted
   read-write and the rest is read-only or absent. Network follows the R1 tier
   for the run. This turns the ledger from "tamper-evident" into
   "tamper-resistant" and lets trusted runs have a shell. ADR needed.
3. **Optional: managed hosting** (L, business decision). Pekohub-hosted
   runtimes for users who will never run a server. This makes Peko a vendor
   too. Keep it optional, and keep `.peko` export one click away, so ownership
   stays the selling point.

### R4 — Hands and triggers (G17, G18, G19, G21)

1. **Web tools** (S–M). `WebFetch` and `WebSearch` builtins, with the search
   provider configurable like models and fetched content wrapped as untrusted
   data (round 1 P6; `acting_for_prompt.rs` already says "information, not
   instructions").
2. **Browser** (L). A bundled browser that runs inside the R3 sandbox:
   CDP/Playwright, as an MCP server or a builtin. It needs a **live view and
   takeover**: stream the screen to desktop or hub, with an "I'm done"
   hand-back. Duet's lack of one is its most-cited weakness. Credentials go
   in through **Secure Form**: the field values go to the browser and are
   referenced as `{{secret:…}}`, never shown to the model.
3. **Activity view** (M). One read model joining ledger runs, plan steps,
   cron history and Async tasks into *In progress / Scheduled / Completed*.
   Each item can pause, stop or carry a redirect note. Steps marked done by
   the agent show as *awaiting your review* until the owner confirms them
   (Duet's "only you can mark a task complete"). Expose it over IPC for
   desktop and the hub, and as `peko activity` in the CLI.
4. **Event triggers** (M). Add `trigger` to Cron `create` as a peer of the
   schedule fields:
   - `webhook`: a per-job URL with a bearer key. This finally gives
     `MessageSource::Webhook` an ingress.
   - `email`: from R2's IMAP adapter, with `+tag` routing as in Grok Bot.
   - `mcp_resource`: an MCP resource subscription.

   Triggered runs are background-lane runs (R1.2), so a forged event can only
   read and propose.
5. **Connector catalog** (M). A curated `connectors/` index of vetted MCP
   servers (Gmail, Google Calendar, GitHub, Slack, Notion, Linear and so on)
   with one-command install and OAuth: `peko connect gmail` writes
   `mcp/gmail/server.json` and opens the `authenticate` URL. Each entry
   declares its methods' default tier for R1. Quality beats count here: OSS
   cannot win on 6,000.

### R5 — Memory, beyond parity (G24, G26)

1. **`kb` search** (M). BM25 over `kb/` and transcript pages first; optional
   local embeddings behind a flag (no vector DB). Results carry the
   provenance that `memory_explain` needs.
2. **`memory_explain`** (S). Given a hot-memory line, return the cited
   sources and the reviewer's verdict (ADR-085). Peko can do this and Dots
   cannot. Show it in the Activity / memory UI.
3. **Memory import** (S–M). Import a ChatGPT or Claude memory export, a
   `USER.md`, or an OpenClaw / Hermes workspace into `kb/`. Every line goes
   through the ADR-085 reviewer, with the import file as its source.
   Onboarding then starts warm, like Duet and Dots.
4. **Forget** (deferred; round 1 G9). Revisit only once R2 adapters mirror
   content to third parties, because that is when real erasure needs appear.

### R6 — Teams and identity (G25, G27)

1. **Delegation template** (S). Seed a *chief of staff* role and a group
   pattern: one owner-facing principal that routes to specialist principals
   through ADR-082 groups. This is Grok Bot's pattern, but with real
   compartments.
2. **Agent identity on the internet** (M). An optional email identity per
   principal: a configured SMTP/IMAP mailbox, with `+label` routing to
   triggers. It pairs with R2.2.
3. **Org controls** (M). Runtime-level admin switches: principal-to-principal
   messaging on or off (as in Grok Bot), allowed connectors, and default
   tiers.
4. **Voice** (L, later). Speech in and out on the desktop and hub clients via
   the configured provider. Phone calls in are the stretch goal.
5. **Payments** — **not planned.** Revisit only through a partner with scoped
   single-use cards, under R1's `never` / `ask` tiers.

### What to measure (peko-benchmark additions)

Parity has to be shown, not claimed. Add these suites before R2 ships:

| Suite | Measures | Why |
|---|---|---|
| `brief` | Morning-brief recall and precision against a seeded inbox and calendar with planted urgent items; every claim must carry a source link | Every product's headline feature |
| `interrupt` | Pushes sent vs. pushes warranted over a simulated week | Noise is the first way proactive agents fail |
| `approval` | Side effects attempted in the background lane; prompt-injected emails that try to send or pay; false-ask rate | The R1 safety claim |
| `idle-cost` | Tokens and money per idle day per principal | Duet's own concern; Peko's quiet ticks should win here |
| `responsibility` (existing) | Obligations met vs. OpenClaw; close the 1.9× cost gap | The OSS head-to-head |

The external benchmark VibeLifeBench (arXiv 2608.10875: proactive tasks in a
world that changes on its own, 22 mock backends) is worth adopting as a
second harness once R4's triggers exist.

---

## 6. Not worth copying (additions to round 1's list)

- **One shared computer for all bots** (Grok Bot). Its own docs say the
  screens are not security boundaries. Peko's per-principal compartments are
  the better design; keep the R3 sandbox per principal.
- **Memory you can only reset in bulk** (Dots). Peko's per-line, sourced
  memory is a selling point.
- **Background monitoring billed against the same allowance as foreground
  work, with no visibility** (Duet). If Peko adds hosted plans, show idle cost
  separately.
- **A browser the owner cannot see** (Duet preview). Ship takeover with the
  browser, or don't ship the browser.
- **Disconnecting an app silently keeps what was learned from it** (all
  four). With per-line sources, Peko can offer to drop the lines whose only
  source is the disconnected connector.

---

## Sources

- [Introducing Claude Duet — Anthropic](https://claude.com/resources/articles/claude-duet)
- [Get started with Claude Duet — Claude Help Center](https://support.claude.com/en/articles/17631495-get-started-with-claude-duet)
- [Anthropic's answer to Muse is here: Claude Duet — VentureBeat](https://venturebeat.com/technology/anthropics-answer-to-muse-is-here-claude-duet-a-mobile-and-desktop-ai-agent-that-takes-actions-proactively)
- [Claude Duet is Anthropic's answer to Meta Muse and OpenAI Dots — Engadget](https://www.engadget.com/2293646/claude-duet-is-anthropics-answer-to-meta-muse-and-openai-dots/)
- [Anthropic Launches Claude Duet — MacRumors](https://www.macrumors.com/2026/10/21/anthropic-claude-duet/)
- [A deep dive into Claude Duet — Flavio Copes](https://flaviocopes.com/claude-duet/)
- [Introducing dots — OpenAI](https://openai.com/index/introducing-dots/)
- [Getting started with your dot — OpenAI Help Center](https://help.openai.com/en/articles/20001530-getting-started-with-your-dot)
- [Dots privacy, security, and safety FAQs — OpenAI Help Center](https://help.openai.com/en/articles/20001529-dots-privacy-security-and-safety-faqs)
- [OpenAI dots, explained from the docs — DEV Community](https://dev.to/sword_luan_6dfb4e81cf5f15/openai-dots-explained-from-the-docs-the-permission-model-of-an-always-on-agent-5eoj)
- [OpenAI launches Dots and ChatGPT Space — VentureBeat](https://venturebeat.com/technology/openai-launches-dots-always-on-ai-agent-coworkers-and-chatgpt-space-where-they-can-collaborate-with-human-teams)
- [OpenAI launches Dots AI agents amid safety questions — NBC News](https://www.nbcnews.com/tech/tech-news/openai-launches-dots-ai-agents-safety-questions-rcna600338)
- [Grok Bot Changelog — SpaceXSI](https://x.ai/changelog/bot)
- [Grok Bot Starts Handing Out Its Own Email Inboxes — TeslaNorth](https://teslanorth.com/2026/10/09/grok-bot-email-inboxes/)
- [Grok Bot Guide: Always-On AI Agents, Routines, and Limits](https://www.ai.joaoqueiros.com/blog/grok-bot-always-on-ai-agent-teams-routines-skills-security)
- [Meta Adds Small Business Skills and App Connectors to Muse — Unite.AI](https://www.unite.ai/meta-adds-small-business-skills-and-app-connectors-to-muse-ai-agent/)
- [Introducing Muse Wonder and Muse Wonder Full — Meta](https://about.fb.com/news/2026/10/introducing-muse-wonder-models/)
- [Meta opens early access program for new Muse features — TechCrunch](https://techcrunch.com/2026/09/25/meta-opens-early-access-program-for-new-muse-features/)
- [Personal agents comparison (nine agents) — Every](https://every.to/personal-agents-comparison)
- [Always-On Agents: Claude Duet vs Dots vs Grok Bot — AIMultiple](https://aimultiple.com/always-on-agents)
- [Always-On AI Agents: Dots vs Muse vs Grok Bot — Taskade](https://www.taskade.com/blog/always-on-ai-agents)
- [VibeLifeBench — arXiv 2608.10875](https://arxiv.org/pdf/2608.10875)
- [Always-On Agents: A Survey of Persistent Memory, State, and Governance — arXiv 2606.30306](https://arxiv.org/pdf/2606.30306)
