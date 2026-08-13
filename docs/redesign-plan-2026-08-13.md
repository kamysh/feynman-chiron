# feynman-chiron redesign plan (2026-08-13)

## Diagnosis

The current implementation is a working RAG-grounded quiz backend wearing an
org-mode costume — the part that made this worth building as an Emacs tool
(a persistent, per-concept learning notebook woven into your notes) was
never actually built.

1. The dialog lives only in a single, shared, ephemeral `*Feynman Chiron*`
   buffer (`feynman-chiron.el`, `feynman-chiron-buffer-name`) — never in the
   `.org` file. Nothing writes it back to any file.
2. `feynman-chiron--process-with-agent` computes
   `(thread-id (or (buffer-file-name) "default"))` with `*Feynman Chiron*`
   as the current buffer — a buffer that never visits a file — so
   `thread-id` is **always** the literal string `"default"`, regardless of
   which org file/concept you're working on. Per-concept progress tracking
   does not actually work.
3. The org file body text is never read by any code path — headings and
   "[Write your explanation here]" placeholders are purely cosmetic.
4. The interaction model itself (read-only, append-only chat log) is a
   questionable fit for a learning notebook, where revising your own
   earlier explanation in place is normal and desirable. User: "I am not a
   big proponent of this read-only/append-only concept — I just did not
   come with anything better."

### One thing that turned out simpler than expected

`chiron-rs`'s backend is **already fully stateless per call** and **already
accepts `concept` and `thread_id` as explicit caller-supplied parameters**
(`agent.rs::process_explanation`). `save_checkpoint` is write-only — nothing
in the codebase ever calls `load_checkpoint`. So the bug is not a backend
protocol limitation; it is entirely a bad value computed on the Elisp side.
**No `chiron-rs` protocol changes are required for the redesign's core
mechanics.**

## Proposed new interaction model

Drop the separate scratch buffer and the read-only/append-only chat log.
Replace it with: **the org heading's subtree IS the session, and stays a
normal, freely-editable org region.**

- One concept = one org heading, at any level, in your normal notes.
- You write and *revise* your explanation directly under that heading —
  normal org editing, no locking, no forced append-only history.
- Submitting (`C-c C-c`, bound by a minor mode active in any org buffer
  with `feynman-chiron-textbook-sources` set — no separate major mode)
  sends the **current full subtree text** (minus Chiron's own inserted
  feedback blocks) as the explanation. Replace, not append — matches how
  Feynman-technique revision actually works, and matches how the backend
  already behaves (stateless per call).
- Chiron's response is inserted as a clearly distinguished but ordinary
  block right there in the subtree — e.g. a
  `#+begin_chiron ... #+end_chiron` block — editable/deletable like any
  org content. Filtered out of "your explanation" on the next submit so
  old feedback is never mistaken for your own words.
- Nothing needs manual saving — it's just your org file; `C-x C-s`
  persists it like everything else you write.

## Fixing session identity

- Replace the broken `thread-id` derivation with a real **org-id**
  (`org-id-get-create`) on the heading — stable across renames/reordering,
  using a mechanism org-mode already provides.
- Concept name comes from the heading text (`org-get-heading`) instead of
  regex-parsing "I'm learning about X" out of user prose.
- Both are already valid backend parameters today — this is purely an
  Elisp-side fix, no `chiron-rs` change needed.
- Mastery/progress checkpoints get keyed on the real org-id. Optionally
  mirror status into an org `PROPERTY` drawer (`:CHIRON_STATUS:`,
  `:CHIRON_SCORE:`) for at-a-glance visibility without querying Postgres.

## Required changes, by layer

**`chiron-rs`**: none required for the core mechanics (see above). Revisit
only if the new Elisp design surfaces a real protocol gap once built.

**`feynman-chiron.el`**:
- New minor mode (e.g. `feynman-chiron-mode`), enabled per-buffer, replacing
  the current derived major mode + separate buffer.
- New submit path operating on the subtree at point: grab heading text +
  subtree text (stripping prior `:CHIRON:`/`#+begin_chiron` blocks), call
  the backend with the real org-id and heading text, insert the response
  block, leave the buffer in normal editable state.
- Remove the now-unnecessary read-only/prompt-marker machinery
  (`--insert-readonly`, `--insert-prompt`, `feynman-chiron-prompt-marker`).
- Backend subprocess management (start/stop `chiron-rs`) stays roughly
  as-is — that's a server-side detail unaffected by the UI redesign.

## What this deliberately drops

- The shared `*Feynman Chiron*` buffer disappears — sessions are
  per-buffer, per-heading, durable by construction.
- Progress currently sitting under the `"default"` thread-id is orphaned
  (it was never usably separated by concept anyway). Clean break, no
  migration — personal single-machine tool, no other users/data at stake.

## Phased build order

1. **Elisp minor mode**: subtree-based submit + non-locking response
   insertion + org-id-based thread identity. (No backend change needed.)
2. **Cleanup**: delete the old buffer/read-only code paths; update
   `README.md`/`WORKFLOW.md` to describe the new usage.
3. **Live verification**: pick one real heading from the
   `~/learning/memory-research/` files, submit an explanation in place,
   confirm it's durably in the file, revise and resubmit, confirm the DB
   checkpoint is keyed on a stable org-id (not `"default"`), restart
   Emacs, confirm state resumes correctly.

## Open item

Confirm this exact interaction model (freely-editable subtree, marked
Chiron blocks, replace-not-append) is what's wanted before implementation
proceeds further, versus a smaller patch that just persists the existing
append-only log into the org file.
