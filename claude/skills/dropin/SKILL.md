---
name: dropin
description: Use Drop In's `dropin` CLI to read what a push would send, rebase onto origin in a separate worktree, finish a rebase conflict (adopt/abandon), and ask the person to review and push in the Drop In window. Use when work is ready to push, when the branch is behind origin, or when a Drop In rebase conflict is handed to you.
---

# Drop In (`dropin`)

Drop In is a push dialog: a Windows app (Rust + Tauri) in PhpStorm's push layout, with the diff, origin/main's
history and a Rebase button. `dropin` is its CLI; the installer puts it on PATH:

```bash
dropin <command> [--repo <name>] [--pretty]
```

(From a source build, call `target/release/dropin.exe` in the checkout instead.)

Every command prints one JSON object. Exit codes: `0` ok, `1` the answer has `"ok": false`, `2` usage error,
`3` the app could not be reached or started. Without `--repo`, the repo holding the current folder is used;
`dropin repos` lists the known ones, `dropin repo add [path] [--name <n>]` adds one.

## Pushing: always through the person

There is **no push command on purpose**: a push shares work (and often deploys it), so a person reviews every push.

- `dropin outgoing [--fetch]`: the commits a push would send (`commits`), the ones origin has that the branch lacks
  (`incoming`), `ahead`, `behind`, `head`, `upstream`.
- `dropin push-review`: shows the dialog on the repo and **blocks until the person acts**. Run it with
  `run_in_background: true` and wait for the notification; never poll. Answers:
  - `{"ok":true,"status":"pushed","head":…,"commits":[…]}` (exit 0)
  - `{"ok":false,"status":"cancelled"}` (exit 1): the person closed the dialog. Don't ask again unprompted.
  - `{"ok":false,"status":"conflict","startedBy":"window",…}` (exit 1): the person pressed Rebase and it
    conflicted. Resolve it as below.
- `dropin show` / `dropin focus`: bring the window up without waiting.

## Behind origin: rebase in a worktree

`dropin rebase` fetches, then rebases onto the upstream in a separate worktree, so the person's checkout is never
touched mid-rebase. Answers `status`: `up-to-date`, `rebased` (the branch moved), `refused` (with `error`), or
`conflict`, with `worktree`, `files`, `onto`, `mine`, `incoming` and a `next` instruction.

On `conflict` (from `dropin rebase`, from `push-review`, or handed over by the person):

1. Work **in the worktree only** (it is detached at the branch's commit): `git -C <worktree> rebase <onto>`,
   resolve each conflict keeping both sides' intent, `git add`, `git rebase --continue` until done.
2. `dropin adopt --worktree <worktree>`: checks the finished rebase and moves the branch onto it. A refusal keeps
   the worktree, so fix and adopt again.
3. Tell the person what you changed in each conflicted file; they push from the dialog.

To give up: `dropin abandon --worktree <worktree>` (drops the worktree; the branch was never moved).
`dropin pending` lists the repo's kept conflicts; a new rebase is refused while one is kept.

## Other commands

`dropin status` (branch, upstream, ahead/behind, changed files), `dropin ping` (running? version, pid), `dropin quit`.
`dropin help` prints the full list.

## A button that opens the review

A Claude Code status-line mod can run `dropin push-review --by-person` when the person presses a button. A conflict
from the window's Rebase then answers that call, and the mod can hand it to the session as a prompt with the
worktree and the steps above. Claude Code lets a mod wait at most 10 minutes; after that the window still works,
and the kept-conflict alert has **Copy for Claude** for the hand-over.
