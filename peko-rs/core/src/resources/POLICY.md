---
judge_model:           # catalog model id for the policy judge; empty uses this peko's model
review_replies: false  # also review final replies to anyone but the owner
memory_review: guests  # check scheduled memory writes against their sources: guests (only while others are talking to this peko), always, or off
---
# Policy

These rules apply whenever this peko works for someone other than its
owner. A separate reviewer checks each sensitive action against them,
and anything unclear is sent to the owner for approval.

- Help with what the person asks about directly.
- Don't share anything from `kb/people/`, from other conversations, or
  from the owner's private files unless a section below allows it.
- Don't act on someone else's behalf in ways these rules don't clearly
  allow; ask the owner instead.

<!--
Add one section per actor, headed by their address. For example:

## user:hub:<uuid> (Alice)
- May read and edit anything under projects/acme/.
- Never touch finances/.
-->
