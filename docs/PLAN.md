# Drop In plan

A standalone Windows app (Rust + Tauri 2, plain HTML/CSS/JS UI, no npm) that gives pushing a real window
where a person reviews every push, plus a CLI Claude calls.

## Shape

- **`dropin-app.exe`**: the window app. One instance; stays resident (tray) so later opens are instant.
- **`dropin.exe`**: a tiny CLI with no Tauri dependency. Talks to the running app over a named pipe,
  starts it when it isn't running, prints one JSON answer, exit code = outcome.
- **Git**: the app runs the real `git.exe`.

## Commands (first cut)

| Command | Returns |
| --- | --- |
| `dropin status [--repo <name>]` | branch, upstream, ahead/behind, at once |
| `dropin push-review [--repo <name>]` | opens the push dialog; waits for Push/Cancel; `{status, pushed}` |
| `dropin update-main [--repo <name>]` | rebase onto upstream in a worktree; `{status: ok\|conflict, worktree, files}` |
| `dropin adopt --worktree <path>` | checks a resolved worktree and moves the branch onto it |
| `dropin focus` | brings the window to the front (taskbar flash as fallback) |
| `dropin repos` / `dropin repo add <path>` | the known repos |

## Multiple repos (requested 2026-10-20)

Quick switching between any number of repos:
- a repo list in the app's config (`%USERPROFILE%\.config\dropin\config.json`; not AppData, which Claude Desktop's MSIX redirects): name, path, remote/branch;
- a repo switcher in the window header (dropdown + keyboard), each repo's state kept while switching;
- every CLI command takes `--repo <name>`; without it, the repo containing the caller's working directory.

## Layout

The foundation is PhpStorm's push dialog. Same structure, cleaner and more modern:
- title bar `Push Commits to <repo>`;
- an alert strip across the top when something needs attention;
- left column: `main → origin : main`, then the commits to push (selected row highlighted);
- right column: a small toolbar, the picked commit's files as a folder tree (`catalog 1 file`),
  and below a divider the commit message, hash, author `<email>` and date;
- footer: help, the primary Push button with a dropdown, Cancel.

Extras, in the same clean style:
- the diff of a clicked file (side-by-side, with a one-column toggle);
- the history of origin/main (the last 20 commits; picking commits lists their files);
- a "behind" alert in the top strip when origin has new commits, with a Rebase button.

## Rebase conflicts

Claude-started: `update-main` answers `conflict` with the worktree; Claude resolves there and calls
`adopt`. Window-started: a waiting `push-review` call answers `conflict`, so whatever opened it can hand
the conflict to a Claude session; with nobody waiting, the alert offers **Copy for Claude**.

## Steps

1. ✅ Toolchain: Rust MSVC (GNU failed), minimal profile. Hello window builds and opens.
2. ✅ App + CLI skeleton: single instance (first pipe instance), per-user pipe `\\.\pipe\dropin-<user>`, JSON line in/out, exit codes 0/1/2/3, `ping`, `status`, `show`, `focus`, `repos`, `repo add|remove`, `quit`, tray, close = hide.
3. ✅ Git logic (pinned reads, overlap check, worktree rebase, `read-tree` + `update-ref`, push the reviewed sha).
   Crate `git/` (package `dropin-git`, no Tauri; `cargo test -p dropin-git`, 13 tests on temp repos). CLI: `outgoing [--fetch]`,
   `rebase` (answers `status: conflict` with the worktree and a `next` instruction), `adopt`, `abandon`, `pending`.
   Kept conflicts survive a restart in `~/.config/dropin/pending.json`; one rebase/adopt per repo at a time.
   Worktrees go in the repo's `.claude/tmp` when it has one, else `~/.cache/dropin/worktrees` (never AppData: MSIX).
   Pushing has no CLI command on purpose: it goes through the dialog, so a person reviews every push.
4. ✅ The dialog UI (`ui/`, plain HTML/CSS/JS): repo switcher, To push + origin/main history, file tree, message,
   split/unified diff with independently scrolling panes, alerts (kept conflict, Claude waiting, behind + Rebase),
   keyboard (Ctrl+Enter push, Alt+↑/↓ files, F5, Esc), light and dark. `focus` never changes the repo; `show` does.
5. ✅ Skill `claude/skills/dropin/SKILL.md` documents the CLI. A Claude Code status-line button can run
   `dropin push-review --by-person` in the background. A Rebase pressed in the window that conflicts answers the
   waiting review `status: conflict, startedBy: window`, so the button's mod can hand it to that session as a
   prompt. Claude Code lets a mod wait 10 minutes at most; after that the window's **Copy for Claude** is the way.
6. ✅ Measured 2026-10-20 (release build): cold `dropin show` to a focused window 656–665 ms; warm `dropin focus` 226–247 ms
   (150 ms of it is the deliberate wait to read whether Windows granted focus); `dropin ping` 60 ms. Window-started
   conflict hand-off tested live on the e2e repo: push-review exited 1 with the conflict after the Rebase click, then
   resolving in the worktree and `dropin adopt` from the repo folder moved main (status `adopted`).
7. ✅ Branches column (requested 2026-10-21, VS Code's Branches tree as the reference): a left column with HEAD, then
   Local and Remote → origin as `/`-prefix folders (closed unless they hold the current branch; `main` pinned first,
   ★ on the remote's default), ↑ahead ↓behind, `gone` and `worktree` marks; the last commit and its full date in the tooltip. Click picks, double-click or Enter
   switches; right-click or the menu key: Switch to, New branch from…, Rename… (F2), Delete (Del), Delete on origin…,
   Copy name. Toolbar: New branch from HEAD (Ctrl+Shift+N) and Fetch every remote (`fetch --all --prune`).
   Ctrl+B or the header button hides it; its width is a gutter like the others.
   Git rules (`git/src/branches.rs`, 7 tests): switching carries uncommitted changes and lets git refuse when one is in
   the way (no stash); a remote branch switches to the local one tracking it, made when missing; a new branch never
   tracks its base (one made from origin/main must not push to main); deleting a branch with commits on no other
   branch asks first (`-d`, then `-D` after a yes); a remote delete always asks and refuses the default branch; a
   branch with a kept rebase conflict cannot be renamed or deleted. Every notice names the command that undoes it.
   Not yet: pushing a new branch the first time (it has no upstream, so the dialog says to `git push -u` from a terminal).
8. ✅ Branch folders (rename, delete, new branch in folder), the diff in its own window (Dock puts it back on the same
   file), and F7 / Shift+F7 to step change by change across a commit's files.
9. ✅ Saved sizes: both windows' size, place and maximized state, every split, the commit list's Author and Age column
   widths, split or unified, docked or undocked, all kept in `~/.config/dropin/ui.json`.
10. ✅ Uncommitted changes: a top row in the commit list with Changes and Unversioned files (tri-state ticks), image
    diffs, Commit / Commit and Push / Amend (off when HEAD is pushed), Rollback. Only the ticked files are committed;
    anything else staged stays staged. 24 git tests in all.
11. ✅ Repo menu: Add Repo… (folder picker) and Remove… (forgets the repo, touches no files). Named Drop In
    on 2026-10-21 (`dropin-app.exe`, `dropin.exe`, crate `dropin-git`, app id `dk.skatepro.dropin`).
12. ✅ Installer and updates: a per-user NSIS installer (no admin) that also installs the CLI and puts it on PATH,
    built and signed by GitHub Actions on a `v*` tag. The header's update button checks the latest GitHub release
    (quietly on start and every 6 hours, marking the button), then downloads, verifies the minisign signature,
    installs and restarts. It refuses while a review waits or a rebase, adopt or push runs.
