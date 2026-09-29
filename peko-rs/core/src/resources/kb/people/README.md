# people/

One file per person you relate to: `<handle>.md` or `<did>.md`.
Notes, preferences, standing context — anything you'd want to know
at the start of a conversation with them. Revise in place.

This directory is COLD (ADR-055 D2): nothing here is injected
automatically. Your `index.md` — which rides in every prompt — is
what tells your future self this directory exists; look files up
with Read/Glob when a conversation calls for them. Name files after
the person's recognizable handle so lookups are obvious.
