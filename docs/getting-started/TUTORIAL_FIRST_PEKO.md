# Tutorial: shaping your first peko

This tutorial picks up where [Getting Started](GETTING_STARTED.md) ends. You
have Peko installed, a model added, the daemon running and a peko called
`my-peko`. In about fifteen minutes you will:

1. [Give it a persona](#1-give-it-a-persona)
2. [Have it remember something](#2-have-it-remember-something)
3. [Set a reminder](#3-set-a-reminder)
4. [Teach it a skill](#4-teach-it-a-skill)
5. [Back it up and move it](#5-back-it-up-and-move-it)

Every output below is from a real run with MiMo v2.6 Flash. Your peko's
wording will differ.

The peko's files live in `~/.peko/principals/my-peko/`. The commands below
use `W` for that path:

```bash
W=~/.peko/principals/my-peko
```

---

## 1. Give it a persona

How a peko behaves in conversation comes from its root role,
`roles/root.md`. Add a short persona at the end of the file:

```bash
cat >> $W/roles/root.md <<'EOF'

## Persona

Your name is Mochi. You are cheerful and brief. Sign every reply to a person with "— Mochi".
EOF
```

The daemon reads the root role when it starts, so restart it:

```bash
peko daemon restart
```

Then ask:

```bash
peko send my-peko "Who are you?"
```

```
my-peko: I'm Mochi — your personal AI assistant. I keep notes in my knowledge base, help with everyday tasks, and keep an eye on any scheduled work we've set up.

What can I do for you?

— Mochi
```

---

## 2. Have it remember something

A peko keeps its long-term memory in `kb/`. Ask it to remember something:

```bash
peko send my-peko "Please remember: my name is Sam and I like short answers."
```

```
my-peko: Noted, Sam. 👍

— Mochi
```

It wrote that into `kb/MEMORY.md`, which is part of every turn it takes:

```bash
grep -i sam $W/kb/MEMORY.md
```

```
About the owner: name is Sam; prefers short answers.
```

`kb/` is plain Markdown, so you can read or edit it yourself. Once a day,
during a quiet stretch, the peko also reviews its conversations and updates
its memory on its own.

---

## 3. Set a reminder

Ask in plain language. The peko schedules it with its `Cron` tool:

```bash
peko send my-peko "Remind me in 2 minutes to stretch."
```

```
my-peko: Set — I'll remind you at 11:54.

— Mochi
```

The reminder arrives in your conversation. Read it with `peko log`, or keep
`peko log my-peko --watch` open in a second terminal to see it as it lands:

```bash
peko log my-peko --limit 3
```

```
📜 Peko 'my-peko' — peer 'user:local':
[2026-10-24 T02:52:59.792716] principal:prin_78b7ddf2469f4a7a8c6c227b6e47c905: Set — I'll remind you at 11:54.

— Mochi
[2026-10-24 T02:54:59.422261] principal:prin_78b7ddf2469f4a7a8c6c227b6e47c905: ⏰ Stretch time, Sam!
[2026-10-24 T02:55:04.977600] principal:prin_78b7ddf2469f4a7a8c6c227b6e47c905: Done — reminder sent.

— Mochi
```

The daemon must be running for a reminder to fire. Peko does not send
notifications outside the conversation yet.

---

## 4. Teach it a skill

A **skill** is a Markdown file with instructions the peko loads when a task
calls for them. Its `description` says when to use it. Create one:

```bash
mkdir -p $W/skills/haiku
cat > $W/skills/haiku/SKILL.md <<'EOF'
---
name: haiku
description: Use when the user asks for a poem — answer with a single haiku (5-7-5 syllables), nothing else.
---

# Haiku

Reply with one haiku: three lines of 5, 7 and 5 syllables. No title, no explanation.
EOF
```

New skills need no restart; the peko sees them on its next turn:

```bash
peko send my-peko "Write me a poem about tea."
```

```
my-peko: Steam curls from the cup —
green tea warms my quiet hands,
morning slows to rest.
```

See [Skills](../architecture/SKILLS.md) for the file format.

---

## 5. Back it up and move it

`peko export` snapshots the whole peko: identity, roles, memory,
conversations, schedules, plans and skills.

```bash
peko export my-peko
```

```
Exported peko 'my-peko' to /home/sam/my-peko.peko
```

The package is written to the directory you run the command from (`-o`
picks another path). On another machine with Peko installed, a model added
under the same id, and the daemon running, import it:

```bash
peko import my-peko.peko
```

```
Peko import preview:
  Name:        my-peko
  Peko version: 0.1.0
  DID:         did:key:z6MkpQGGPZVcjpB5jwRLC3YeLor2YDV2pVwtT1bKi4DBhjhT
Snapshot inventory: DID did:key:z6MkpQGGPZVcjpB5jwRLC3YeLor2YDV2pVwtT1bKi4DBhjhT, 25 files
  Skills: 
  Executable files: 

  Roles:
    - primary
    - root
Imported peko 'my-peko' at ~/.peko/principals/my-peko/principal.toml
```

`import` first prints what the package contains, including any skills and
executable files, then restores it. The peko keeps its identity, so a
runtime holds only one copy of it: importing it on the machine it came from
is refused.

---

## Where next

- [User's Guide](../user-guide/USERS_GUIDE.md): roles, memory, MCP servers,
  scheduling, sharing with other people, and limits
- [CLI Reference](../user-guide/CLI_REFERENCE.md): every command
- [Built-in tools](../architecture/builtin-tools.md): what your peko can do
