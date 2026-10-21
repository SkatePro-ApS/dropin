# Drop In: notes for Claude

Read [README.md](README.md) first. It covers the layout, the CLI protocol, the git rules and where files live.
[docs/PLAN.md](docs/PLAN.md) records why each step was built the way it was.

## Build and test

- Build: `cargo build --release` (3–4 min). Run `target/release/dropin.exe quit` first, because a running exe can't
  be overwritten.
- Git tests: `cargo test -p dropin-git`. Any change to `git/` gets a test in `git/src/tests.rs`.
- In Git Bash, cargo may not be on PATH: `export PATH="$HOME/.cargo/bin:$PATH"`.
- Installer: `cargo tauri build -c '{"bundle":{"createUpdaterArtifacts":false}}'` from `app/`. Releases: README → Releasing.

## UI testing: silently, never on the real desktop

- Never drive the real mouse or keyboard, and never steal focus. Start the app hidden with
  `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9333 target/release/dropin-app.exe --background`
  and drive it with `node tools/cdp.js …`.
- Never trigger **Add Repo…** (`repo_pick`) in a test. It opens a real folder dialog on the person's screen, and
  `__TAURI_INTERNALS__.invoke` can't be monkeypatched.
- Commit, delete and push tests run on a throwaway repo, never on a real one.

## Rules

- There is no push command in the CLI, on purpose: a push shares work (and often deploys it), so a person reviews
  every push in the window. Don't add one.
- Don't test the update button's install on the real desktop: it exits the app and runs the installer. Check
  `update_check` only, and test installs in a release you can uninstall.
- Never commit signing keys. The updater's private key lives only in the repo's Actions secrets and offline.
- Config and caches stay under `%USERPROFILE%\.config\dropin` and `.cache\dropin`, never `%APPDATA%`: Claude
  Desktop's MSIX package redirects AppData writes.
- Keep the CLI free of Tauri and other heavy dependencies; it has to start in milliseconds.
- UI: plain HTML/CSS/JS, no npm, no framework.
