# Why these are not auto-loaded

`CLAUDE.md` used to `@`-load all four documents in this directory. It no longer
does, and this file is the reason, because a reader who notices the change should
find an argument rather than an absence.

## The measurement

Before: **42,300 tokens** entered every session before the user had said anything —
CLAUDE.md at 2,800 plus the four documents at 39,500. After: **834**.

Nothing was deleted to get there. The documents are the same documents; they are
read when the task touches them.

## The argument

Context is a finite attention budget and every token spends some of it. The
guidance this follows is Anthropic's — *the smallest possible set of high-signal
tokens that maximize the likelihood of the desired outcome* — and the measured
result behind it is uncomfortable: agents given a 100K-token codebase summary
performed **worse** than agents given 5K tokens of targeted retrieval. Twenty times
the context, worse answers. The AGENTS.md convention puts the root context file at
twenty to thirty lines for the same reason, and notes that duplicated content
measurably hurts.

So the split is by *when you need it*, not by *how important it is*:

| | |
|---|---|
| **Eager** — in `CLAUDE.md` | What you must know **before you know you need it**. The invariants, which are short, catastrophic when broken, and silent. Nobody opens a document to check whether a gain stage is allowed; they add one. |
| **On demand** — here | Everything you would **look up**. Pin numbers, ALSA flags, the format table, the reasoning behind a decision. You already know to ask; the map in `CLAUDE.md` says where. |

The failure this trades against is real and worth naming: this project's defect
shape is the silent wrong thing, and part of its defence was that the caution had
already been read. That is why the boundary is drawn where it is rather than at
"keep it short" — the invariant list is precisely the set of cautions you would not
have thought to seek.

## The documents

| | Open it when |
|---|---|
| `hardware.md` | Any pin, jumper, connector, power or bring-up question. Carries the 40-pin header map. |
| `architecture.md` | The design — format scope, ring and window, transport, threading, v2. Should be stable. |
| `implementation.md` | What to type and what fails silently. Includes *What reads as handled and is not*, the checklist for this project's recurring defect. |
| `decisions.md` | Before changing a design choice. Settled decisions and, more importantly, the reversed ones — whose superseded reasoning has no other home. |

Open questions are [GitHub issues](https://github.com/tamatebox/deck-pi/issues),
with their analysis, deliberately outside the context by default: an undecided
question is the last thing that should be loaded before it is asked about.
