# Why these are not auto-loaded

`CLAUDE.md` used to `@`-load every document in this directory. It no longer does,
because a reader who notices should find an argument rather than an absence.

## The measurement

These documents total about **35,000 words**, and all of it used to enter every
session before the user had said anything. Now one of them does: `CLAUDE.md` and the
document it imports, `this-build.md`, are the only always-loaded files, at
**173 lines** together. Check both with `wc`; nothing was deleted to get there, and
the rest are read when the task touches them.

## The argument

Context is a finite attention budget, and the guidance is Anthropic's own:

- **"Target under 200 lines per CLAUDE.md file. Longer files consume more context and
  reduce adherence."** Also: *"shorter files produce better adherence."*
- **"Find the smallest possible set of high-signal tokens that maximize the
  likelihood of some desired outcome."**
- **Context rot** — "as the number of tokens in the context window increases, the
  model's ability to accurately recall information from that context decreases."
- Two measured results that are less comfortable. Agents handed a **100K-token
  codebase summary performed worse** than agents handed 5K tokens of targeted
  retrieval. And **LLM-generated context files reduced task success while adding over
  20% to inference cost**; developer-written ones gave a small lift, but only when
  minimal and precise. **These documents are LLM-generated**, which is the strongest
  argument here for keeping them out of the default context and for keeping them
  short.

`/doctor`'s own trim rule is the operational version, and it is better than a word
count: **cut what can be derived from the codebase** — directory layouts, dependency
lists, architecture overviews — and **keep pitfalls, rationale, and conventions that
differ from tool defaults.**

So the split is by *when you need it*, not by *how important it is*:

| | |
|---|---|
| **Eager** — `CLAUDE.md`, and `this-build.md` through it | What you must know **before you know you need it**: the invariants, which are short, catastrophic when broken, and silent. Nobody opens a document to check whether a gain stage is allowed; they add one. **Which parts exist is the same kind of fact** — nobody looks up whether the encoder is wired; they assume it is. |
| **On demand** — here | Everything you would **look up**. Pin numbers, ALSA flags, the format table, the reasoning behind a decision. You already know to ask, and `CLAUDE.md`'s map says where. |

## Which document

**`CLAUDE.md`'s map already says**, one trigger per file, and it is the copy that is
always loaded. A second copy here would be a second thing to keep current — the exact
failure this split exists to avoid.

Open questions are [GitHub issues](https://github.com/tamatebox/deck-pi/issues), with
their analysis, deliberately outside the context by default: an undecided question is
the last thing that should be loaded before it is asked about.
