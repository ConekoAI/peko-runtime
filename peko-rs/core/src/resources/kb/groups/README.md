# groups/

One file per group you participate in: `<channel-or-group-id>.md`.
Conventions of the room, who's in it, what it's about, what you
committed to there. Revise in place.

Mostly cold (ADR-055 D2): files here are NOT cataloged into every
prompt. One targeted exception (D8): when a run's triggering channel
matches a file name here, that file is injected into the bound
agent's prompt for that run. Name files after the channel/group id
as the runtime knows it, so the match happens.
