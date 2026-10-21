// The push dialog: PhpStorm's layout (commits left, files and message right), plus the diff of a
// clicked file, the upstream's history, and alerts for what needs doing before a push.
//
// Every repo keeps its own view (lists, picks, notices) while you switch between them.

import { DiffView, WORK } from './diffview.js';

const $ = id => document.getElementById(id);

/** A new element with a class and text. */
function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined && text !== null) node.textContent = text;
  return node;
}

const plural = (n, word) => `${n} ${word}${n === 1 ? '' : 's'}`;

/**
 * The page's settings, kept by the app in `~/.config/dropin/ui.json` (prefs.rs says why not in
 * localStorage). Read once when the page starts; each change is written at once.
 */
const saved = {
  values: {},
  async load() {
    this.values = await api.invoke('prefs').catch(() => ({}));
  },
  get(key, fallback) {
    return key in this.values ? this.values[key] : fallback;
  },
  set(key, value) {
    this.values[key] = value;
    api.invoke('set_pref', { key, value }).catch(() => {});
  },
};

/** The app's commands; outside Tauri (a preview in a plain browser) the preview server's mock.js stands in. */
async function connect() {
  const tauri = window.__TAURI__;
  if (!tauri) return (await import('./mock.js')).api;
  const win = tauri.window.getCurrentWindow();
  return {
    invoke: tauri.core.invoke,
    listen: tauri.event.listen,
    emitTo: tauri.event.emitTo,
    setTitle: title => win.setTitle(title).catch(() => {}),
  };
}

let api;
let repos = [];
let current = null;
/** Side by side or unified; read from the settings when the page starts. */
let layout = 'split';
let busyReads = 0;

/** Per repo: what it shows and what is picked. */
const views = new Map();
function view(name) {
  if (!views.has(name)) {
    views.set(name, {
      data: null,
      token: 0,
      inflight: 0,
      readAt: 0,
      fetchedAt: 0,
      sel: null,
      files: null,
      filesSha: null,
      filesError: null,
      file: null,
      diffOpen: false,
      closed: new Set(),
      cursor: null,
      notice: null,
      busy: null,
      branches: null,
      branchesError: null,
      branchesToken: 0,
      branchCursor: null,
      // Branch tree nodes the person opened or closed against their default.
      branchOpen: new Map(),
      // The Uncommitted row: the work tree's changes, read with the lists; `workStamp` counts the
      // reads, so a diff of an uncommitted file is read again after each.
      work: null,
      workError: null,
      workToken: 0,
      workStamp: 0,
      // Paths ticked for the next commit, and every path listed so far (a new one gets its default tick).
      ticked: new Set(),
      seen: new Set(),
      draft: '',
      amend: false,
      // The message written before Amend put HEAD's in its place, back when Amend is unticked.
      amendSaved: '',
    });
  }
  return views.get(name);
}

/** Commits never change, so their file lists are kept for the app's life. */
const filesCache = new Map();

// ---------------------------------------------------------------- reading

/** Reads the repo's lists; `fetch` asks git to fetch the upstream first. */
async function read(name, fetch = false) {
  const v = view(name);
  const token = ++v.token;
  v.inflight++;
  progress(+1);
  // Listing branches takes a few ms; after a fetch it runs again for the new ahead/behind counts.
  readBranches(name);
  const working = readWork(name);
  try {
    const data = await api.invoke('outgoing', { name, fetch });
    if (fetch) readBranches(name);
    if (token !== v.token) return;
    v.data = data;
    v.readAt = Date.now();
    if (fetch && !data.fetchError && !data.error) v.fetchedAt = Date.now();
    keepSelection(v);
  } catch (error) {
    if (token !== v.token) return;
    v.data = { error: String(error), kept: [] };
  } finally {
    await working;
    v.inflight--;
    progress(-1);
  }
  if (name === current) {
    renderAll();
    await loadFiles();
  }
}

/**
 * Reads the work tree's uncommitted changes. A file listed for the first time starts ticked when
 * git tracks it, as in PhpStorm; a new file waits to be ticked, so a stray one is not committed.
 */
async function readWork(name) {
  const v = view(name);
  const token = ++v.workToken;
  try {
    const work = await api.invoke('changes', { name });
    if (token !== v.workToken) return;
    const now = new Set(work.files.map(f => f.path));
    for (const f of work.files) {
      if (v.seen.has(f.path)) continue;
      v.seen.add(f.path);
      if (f.status !== '?') v.ticked.add(f.path);
    }
    for (const path of [...v.seen]) {
      if (now.has(path)) continue;
      v.seen.delete(path);
      v.ticked.delete(path);
    }
    v.work = work;
    v.workError = null;
  } catch (error) {
    if (token !== v.workToken) return;
    v.work = null;
    v.workError = String(error);
  }
  v.workStamp++;
}

function progress(step) {
  busyReads += step;
  $('progress').hidden = busyReads <= 0;
}

/** Every commit the list shows, in its order: outgoing first, then the upstream's history. */
function listed(data) {
  if (!data || data.error) return [];
  return [...(data.commits ?? []), ...(data.history ?? [])];
}

/** Keeps the picked commit (or the Uncommitted row) when it is still listed; else picks the first. */
function keepSelection(v) {
  if (v.sel === WORK) return;
  const all = listed(v.data);
  if (!all.some(c => c.sha === v.sel)) v.sel = all[0]?.sha ?? WORK;
}

/** The commit list's rows in order, as ↑/↓ step through them: the Uncommitted row first. */
function rowOrder(v) {
  return [WORK, ...listed(v.data).map(c => c.sha)];
}

/** The uncommitted files ticked for the next commit, in the list's order. */
function tickedFiles(v) {
  return (v.work?.files ?? []).filter(f => v.ticked.has(f.path));
}

async function loadFiles() {
  const name = current;
  const v = view(name);
  const sha = v.sel;
  if (!sha) {
    v.files = [];
    v.filesSha = null;
    renderDetail();
    renderDiff();
    return;
  }
  if (sha === WORK) {
    // Read with the lists, so always current.
    v.files = v.work?.files ?? [];
    v.filesError = v.workError;
    v.filesSha = WORK;
    keepOpenFile(v);
  } else if (v.filesSha !== sha) {
    const key = `${name}\n${sha}`;
    if (!filesCache.has(key)) {
      filesCache.set(key, api.invoke('files', { name, sha }).catch(error => {
        filesCache.delete(key);
        throw error;
      }));
    }
    try {
      const files = await filesCache.get(key);
      if (current !== name || v.sel !== sha) return;
      v.files = files;
      v.filesError = null;
    } catch (error) {
      if (current !== name || v.sel !== sha) return;
      v.files = [];
      v.filesError = String(error);
    }
    v.filesSha = sha;
    keepOpenFile(v);
  }
  renderDetail();
  renderDiff();
}

/** A file the new commit did not touch has no diff here: show the commit's first file instead. */
function keepOpenFile(v) {
  if (v.files.some(f => f.path === v.file)) return;
  v.file = v.diffOpen ? (visibleFiles(v)[0]?.path ?? null) : null;
  if (v.file || v.cursor?.startsWith('f:')) v.cursor = v.file ? `f:${v.file}` : null;
  if (!v.file) v.diffOpen = false;
}

// ---------------------------------------------------------------- repos

/** Reads the repo list and shows `select`, else the repo shown now, else the last one used. */
async function loadRepos(select, fetch = false) {
  repos = await api.invoke('list_repos');
  if (!repos.length) {
    current = null;
    $('repo-name').textContent = 'No repo';
    renderAll();
    return;
  }
  const want = [select, current, saved.get('repo', null)].find(n => n && repos.some(r => r.name === n)) ?? repos[0].name;
  await switchTo(want, fetch);
}

/** Set while the menu's own add or remove runs: it reads the list itself. */
let ownChange = false;

/**
 * `repos-changed`: the list changed. Skipped for the menu's own add or remove, and when the
 * list is unchanged and shows the wanted repo already.
 */
async function reposChanged(select) {
  if (ownChange) return;
  const fresh = await api.invoke('list_repos');
  if (JSON.stringify(fresh) === JSON.stringify(repos) && current && (!select || select === current)) return;
  await loadRepos(select);
}

/** The repo button's menu: switch, add, remove. */
function repoMenu() {
  const box = $('repo').getBoundingClientRect();
  const busy = Boolean(current && view(current).busy);
  openMenu(box.left, box.bottom + 4, [
    ...repos.map((r, i) => ({
      label: r.name,
      key: i < 9 ? `Ctrl+${i + 1}` : '',
      title: r.path,
      checked: r.name === current,
      run: () => r.name !== current && switchTo(r.name),
    })),
    ...(repos.length ? [{ sep: true }] : []),
    { label: 'Add Repo…', run: addRepo },
    {
      label: current ? `Remove ${current}…` : 'Remove…',
      title: busy ? `${current} is busy; wait for it to finish.` : 'Take it off this list; nothing on disk changes.',
      disabled: !current || busy,
      run: removeRepo,
    },
  ], $('repo'));
}

/** Picks a folder with Windows' own dialog, then asks for the name it goes by. */
async function addRepo() {
  let picked;
  try {
    picked = await api.invoke('repo_pick', { near: current });
  } catch (error) {
    settle({ tone: 'error', text: String(error) });
    return;
  }
  if (!picked) return;
  await ask({
    title: 'Add repo',
    text: 'The name shows in the repo menu, and is what dropin --repo takes.',
    input: picked.name,
    placeholder: 'Name',
    detail: picked.path,
    ok: 'Add',
    run: async value => {
      const name = value.trim();
      ownChange = true;
      try {
        await api.invoke('repo_add', { path: picked.path, name });
        await loadRepos(name);
      } catch (error) {
        return { tone: 'error', text: String(error) };
      } finally {
        ownChange = false;
      }
      return { tone: 'success', text: `Added ${name}.` };
    },
  });
}

/** Takes the repo shown off the list, after asking; its files stay as they are. */
async function removeRepo() {
  const repo = repos.find(r => r.name === current);
  if (!repo) return;
  await ask({
    title: `Remove ${repo.name}?`,
    text: 'Drop In forgets it. Nothing on disk changes, and you can add it again any time.',
    detail: repo.path,
    ok: 'Remove',
    run: async () => {
      ownChange = true;
      try {
        await api.invoke('repo_remove', { name: repo.name });
        views.delete(repo.name);
        await loadRepos();
      } catch (error) {
        return { tone: 'error', text: String(error) };
      } finally {
        ownChange = false;
      }
      return { tone: 'success', text: `Removed ${repo.name} from the list.` };
    },
  });
}

/** Shows a repo: its kept view at once, then fresh lists (a fetch the first time). */
async function switchTo(name, fetch = false) {
  current = name;
  $('repo-name').textContent = name;
  saved.set('repo', name);
  api.setTitle(`Push Commits to ${name}`);
  const v = view(name);
  renderAll();
  if (v.data) loadFiles();
  if (!v.data && !fetch) {
    // The local read answers in a few hundred ms; the fetch can take seconds, so it comes second.
    await read(name, false);
    read(name, true);
  } else {
    read(name, fetch || !v.fetchedAt);
  }
}

// ---------------------------------------------------------------- actions

function canPush(v) {
  const d = v.data;
  return Boolean(d && !d.error && d.ahead > 0 && d.behind === 0 && !v.busy && !d.kept?.length);
}

async function push() {
  const name = current;
  const v = view(name);
  if (!canPush(v)) return;
  v.busy = 'push';
  v.notice = null;
  renderAll();
  let refetch = false;
  try {
    const pushed = await api.invoke('push', { name, head: v.data.head });
    v.notice = pushed.notice;
    refetch = pushed.refetch;
  } catch (error) {
    v.notice = { tone: 'error', text: String(error) };
  }
  v.busy = null;
  await read(name, refetch);
}

async function rebase() {
  const name = current;
  const v = view(name);
  if (v.busy) return;
  v.busy = 'rebase';
  v.notice = null;
  renderAll();
  try {
    const r = await api.invoke('rebase', { name });
    if (r.status === 'up-to-date') {
      v.notice = { tone: 'success', text: `${r.branch} already has everything on ${r.upstream}.` };
    } else if (r.status === 'conflict') {
      // A waiting review is answered with the conflict, so the session that opened the window resolves it.
      const handed = {
        claude: ' Claude, who opened this review, has been asked to resolve it.',
        person: ' The Claude session you opened this from has been asked to resolve it.',
      }[v.data?.waiting] ?? '';
      v.notice = {
        tone: 'warning',
        text: `The rebase stopped on a conflict in ${plural(r.files.length, 'file')}. Your branch was not touched; the rebase waits in its worktree.${handed}`,
      };
    } else {
      v.notice = r.notice ?? { tone: r.ok ? 'success' : 'error', text: r.error ?? r.status };
    }
    v.fetchedAt = Date.now();
  } catch (error) {
    v.notice = { tone: 'error', text: String(error) };
  }
  v.busy = null;
  await read(name, false);
}

async function adopt(kept) {
  const name = current;
  const v = view(name);
  if (v.busy) return;
  v.busy = 'adopt';
  renderAll();
  try {
    const r = await api.invoke('adopt', { name, worktree: kept.worktree });
    v.notice = r.notice;
  } catch (error) {
    v.notice = { tone: 'error', text: String(error) };
  }
  v.busy = null;
  await read(name, false);
}

async function abandon(kept) {
  const name = current;
  const v = view(name);
  if (v.busy) return;
  v.busy = 'abandon';
  renderAll();
  try {
    await api.invoke('abandon', { name, worktree: kept.worktree });
    v.notice = { tone: 'success', text: 'Dropped the kept rebase and its worktree. The branch was never touched.' };
  } catch (error) {
    v.notice = { tone: 'error', text: String(error) };
  }
  v.busy = null;
  await read(name, false);
}

/** The instruction Claude needs to finish a kept rebase, copied for pasting into a session. */
async function copyForClaude(kept) {
  const text = [
    `Finish the rebase Drop In kept for ${kept.repo}: ${kept.branch} onto ${kept.upstream} stopped on a conflict in ${kept.files.join(', ')}.`,
    `The worktree is ${kept.worktree} (detached at the branch's commit). There: git rebase ${kept.onto},`,
    `resolve each conflict keeping both sides' intent, git add, git rebase --continue until done.`,
    `Then run: dropin adopt --repo ${kept.repo} --worktree "${kept.worktree}"`,
  ].join(' ');
  const v = view(current);
  try {
    await navigator.clipboard.writeText(text);
    v.notice = { tone: 'success', text: 'Copied the instruction for Claude. Paste it into a session.' };
  } catch {
    v.notice = { tone: 'warning', text: 'Could not reach the clipboard. The instruction:', detail: text };
  }
  renderAlerts();
}

function cancel() {
  api.invoke('cancel').catch(() => {});
}

// ---------------------------------------------------------------- rendering

function renderAll() {
  renderHeader();
  renderAlerts();
  renderBranches();
  renderList();
  renderDetail();
  renderDiff();
  renderFooter();
}

function renderHeader() {
  const route = $('route');
  route.replaceChildren();
  const d = current ? view(current).data : null;
  if (d && d.branch) {
    route.append(el('span', 'branch', d.branch), el('span', 'arrow', '→'));
    if (d.preview) {
      route.append(el('span', 'remote', d.preview));
    } else if (d.remote) {
      route.append(el('span', 'remote', d.remote), el('span', 'sep', ':'), el('span', 'branch', d.remoteBranch));
    }
  }
  renderFetched();
}

function renderFetched() {
  const v = current ? view(current) : null;
  $('fetched').textContent = v && v.fetchedAt ? `fetched ${ago(v.fetchedAt)}` : '';
}

/** "just now", "4 min ago", "2 h ago". */
function ago(ms) {
  const s = Math.round((Date.now() - ms) / 1000);
  if (s < 45) return 'just now';
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  return `${Math.round(s / 3600)} h ago`;
}

/** A commit's age for the list: "5m", "3h", "2d", "14 Oct". */
function age(at) {
  const s = (Date.now() - at) / 1000;
  if (s < 60) return 'now';
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  if (s < 86400 * 7) return `${Math.floor(s / 86400)}d`;
  return new Date(at).toLocaleDateString('en-GB', { day: 'numeric', month: 'short' });
}

function renderAlerts() {
  const box = $('alerts');
  box.replaceChildren();
  if (updateNotice) {
    box.append(dismissable(updateNotice, () => {
      updateNotice = null;
      renderAlerts();
    }));
  }
  if (!current) return;
  const v = view(current);
  const d = v.data ?? {};
  const kept = d.kept ?? [];
  for (const k of kept) {
    const text = el('div', 'text');
    text.append(
      `A rebase of ${k.branch} onto ${k.upstream} stopped on a conflict in ${plural(k.files.length, 'file')}: `,
      el('code', '', k.files.join(', ')),
      '. Finish it in ',
      el('code', '', k.worktree),
      ', then adopt it. Pushing waits until then.',
    );
    box.append(alert('error', '!', text, [
      { label: 'Copy for Claude', run: () => copyForClaude(k) },
      { label: 'Abandon', run: () => abandon(k) },
      { label: v.busy === 'adopt' ? 'Adopting…' : 'Adopt', strong: true, run: () => adopt(k) },
    ]));
  }
  if (d.waiting === 'claude') {
    box.append(alert('info', 'C', 'Claude is waiting on this review. Push answers it; Cancel tells it you did not push.'));
  }
  if (d.behind > 0 && !kept.length) {
    const text = `${d.upstream} has ${plural(d.behind, 'commit')} this branch lacks (marked • below). Rebase onto ${d.upstream} before pushing.`;
    box.append(alert('warning', '↓', text, [
      { label: v.busy === 'rebase' ? 'Rebasing…' : 'Rebase', strong: true, run: rebase },
    ]));
  }
  if (d.fetchError) {
    box.append(alert('warning', '!', 'Fetching the upstream failed, so the lists may be out of date.', [], d.fetchError));
  }
  if (d.error) box.append(alert('error', '!', d.error));
  if (v.notice) {
    box.append(dismissable(v.notice, () => {
      v.notice = null;
      renderAlerts();
    }));
  }
}

/** A notice's alert row with a close button. */
function dismissable(notice, dismiss) {
  const badge = notice.tone === 'success' ? '✓' : '!';
  const row = alert(notice.tone, badge, notice.text, [], notice.detail);
  const close = el('button', 'icon-button dismiss');
  close.title = 'Dismiss';
  close.innerHTML = '<svg class="icon" viewBox="0 0 16 16"><path d="M4 4l8 8 M12 4l-8 8" /></svg>';
  close.onclick = dismiss;
  row.append(close);
  return row;
}

/** One alert row: a badge, the text (a string or a node), optional buttons and git's detail. */
function alert(tone, badge, text, actions = [], detail = '') {
  const row = el('div', `alert ${tone}`);
  const body = typeof text === 'string' ? el('div', 'text', text) : text;
  // git's output can carry blank lines (progress it redrew in place): show only the text.
  const shown = detail ? detail.trim().replace(/\n\s*\n/g, '\n') : '';
  if (shown) body.append(el('pre', 'git-said', shown));
  row.append(el('span', 'badge', badge), body);
  if (actions.length) {
    const box = el('div', 'actions');
    const busy = Boolean(current && view(current).busy);
    for (const action of actions) {
      const button = el('button', action.strong ? 'strong' : '');
      button.append(el('span', '', action.label));
      button.disabled = busy;
      button.onclick = action.run;
      box.append(button);
    }
    row.append(box);
  }
  return row;
}

function renderList() {
  const list = $('list');
  list.replaceChildren();
  if (!current) {
    const empty = el('div', 'empty');
    const add = el('button', 'primary', 'Add Repo…');
    add.type = 'button';
    add.onclick = addRepo;
    empty.classList.add('start');
    const hint = el('p', 'hint');
    hint.append('Or run ', el('code', 'mono', 'dropin repo add'), ' in a terminal in the repo.');
    empty.append(el('strong', '', 'No repos yet.'), add, hint);
    list.append(empty);
    return;
  }
  const v = view(current);
  const d = v.data;
  if (!d) return;
  list.append(workRow(v));
  if (d.error) {
    list.append(el('div', 'empty', 'Nothing to list until the problem above is fixed.'));
    list.querySelector('.row.selected')?.scrollIntoView({ block: 'nearest' });
    return;
  }

  const head = (title, n) => {
    const h = el('div', 'list-head', title);
    if (n !== undefined) h.append(el('span', 'n', n));
    return h;
  };
  list.append(head(d.preview ? `After ${d.preview}` : 'To push', d.commits.length ? String(d.ahead) : undefined));
  if (!d.commits.length) {
    const empty = el('div', 'empty');
    empty.append(el('strong', '', 'Nothing to push. '), `${d.branch} has no commits ${d.upstream} lacks.`);
    list.append(empty);
  }
  for (const c of d.commits) list.append(commitRow(v, c, 'outgoing'));

  if (d.historyRef) {
    list.append(head(d.historyRef, d.history.length ? `last ${d.history.length}` : undefined));
    if (d.historyError) list.append(el('div', 'empty', d.historyError));
    const incoming = new Set((d.incoming ?? []).map(c => c.sha));
    for (const c of d.history) list.append(commitRow(v, c, incoming.has(c.sha) ? 'history incoming' : 'history'));
  }
  list.querySelector('.row.selected')?.scrollIntoView({ block: 'nearest' });
}

function commitRow(v, c, kind) {
  const row = el('div', `row ${kind}`);
  row.setAttribute('role', 'option');
  row.dataset.sha = c.sha;
  row.title = `${c.short} · ${c.author} · ${c.date}`;
  if (kind.includes('incoming')) row.append(el('span', 'dot'));
  row.append(el('span', 'subject', c.subject), el('span', 'author', c.author), el('span', 'age', age(c.at)));
  if (c.sha === v.sel) {
    row.classList.add('selected');
    row.setAttribute('aria-selected', 'true');
  }
  row.onclick = () => pickCommit(c.sha);
  return row;
}

const PENCIL = '<svg class="icon" viewBox="0 0 16 16"><path d="M10.5 2.5l3 3-8 8h-3v-3z M9 4l3 3" /></svg>';

/** The Uncommitted row above the commits: the work tree's changes, to pick files from and commit. */
function workRow(v) {
  const files = v.work?.files ?? [];
  const row = el('div', `row work${files.length ? '' : ' clean'}`);
  row.setAttribute('role', 'option');
  row.dataset.sha = WORK;
  const subject = el('span', 'subject');
  subject.innerHTML = PENCIL;
  if (v.workError) {
    subject.append(el('span', 'text', 'Uncommitted changes could not be read'));
    row.title = v.workError;
  } else if (files.length) {
    const untracked = files.filter(f => f.status === '?').length;
    subject.append(el('span', 'text', 'Uncommitted changes'), el('span', 'n', String(files.length)));
    row.title = untracked ? `${plural(files.length - untracked, 'changed file')}, ${plural(untracked, 'unversioned file')}` : plural(files.length, 'changed file');
  } else {
    subject.append(el('span', 'text', 'No uncommitted changes'));
  }
  row.append(subject, el('span', 'author'), el('span', 'age'));
  if (v.sel === WORK) {
    row.classList.add('selected');
    row.setAttribute('aria-selected', 'true');
  }
  row.onclick = () => pickCommit(WORK);
  return row;
}

function pickCommit(sha) {
  const v = view(current);
  if (v.sel === sha) return;
  v.sel = sha;
  for (const row of $('list').querySelectorAll('.row')) {
    const on = row.dataset.sha === sha;
    row.classList.toggle('selected', on);
    row.setAttribute('aria-selected', String(on));
    if (on) row.scrollIntoView({ block: 'nearest' });
  }
  renderMessage();
  loadFiles();
}

function selectedCommit(v) {
  return listed(v.data).find(c => c.sha === v.sel) ?? null;
}

function renderDetail() {
  renderTree();
  renderMessage();
}

/** Files as nested folders; a folder with one folder and no files merges into it (`a/b`). */
function buildTree(files) {
  const root = { name: '', path: '', dirs: new Map(), files: [] };
  for (const file of files) {
    const parts = file.path.split('/');
    const name = parts.pop();
    let node = root;
    let path = '';
    for (const part of parts) {
      path = path ? `${path}/${part}` : part;
      if (!node.dirs.has(part)) node.dirs.set(part, { name: part, path, dirs: new Map(), files: [] });
      node = node.dirs.get(part);
    }
    node.files.push({ file, name });
  }
  const merge = dir => {
    for (const [key, child] of dir.dirs) {
      let node = child;
      while (node.dirs.size === 1 && node.files.length === 0) {
        const only = node.dirs.values().next().value;
        node = { ...only, name: `${node.name}/${only.name}` };
      }
      dir.dirs.set(key, node);
      merge(node);
    }
  };
  merge(root);
  return root;
}

const countOf = dir => dir.files.length + [...dir.dirs.values()].reduce((n, d) => n + countOf(d), 0);
const byName = (a, b) => a.name.localeCompare(b.name, undefined, { sensitivity: 'base' });

/**
 * The Uncommitted row's file groups, as PhpStorm's commit view has them: changes to tracked files,
 * then files git does not track yet. Empty groups are left out.
 */
function workGroups(v) {
  const files = v.files ?? [];
  return [
    { id: 'c', label: 'Changes', files: files.filter(f => f.status !== '?') },
    { id: 'u', label: 'Unversioned files', files: files.filter(f => f.status === '?') },
  ].filter(g => g.files.length);
}

/**
 * One file tree's rows in drawing order, folders first; closed folders hide their contents. A
 * folder's `id` (what `closed` holds) is its path, after `scope` in a group of the Uncommitted row.
 */
function treeRows(v, files, scope, depth) {
  const rows = [];
  const walk = (dir, depth) => {
    for (const child of [...dir.dirs.values()].sort(byName)) {
      const id = `${scope}${child.path}`;
      rows.push({ kind: 'dir', key: `d:${id}`, id, dir: child, depth });
      if (!v.closed.has(id)) walk(child, depth + 1);
    }
    for (const { file, name } of [...dir.files].sort(byName)) {
      rows.push({ kind: 'file', key: `f:${file.path}`, file, name, depth });
    }
  };
  walk(buildTree(files), depth);
  return rows;
}

/** The tree's rows in drawing order: under a heading per group for the Uncommitted row. */
function visibleRows(v) {
  if (v.sel !== WORK) return treeRows(v, v.files ?? [], '', 0);
  const rows = [];
  for (const group of workGroups(v)) {
    const id = `g:${group.id}`;
    rows.push({ kind: 'group', key: id, id, group, depth: 0 });
    if (!v.closed.has(id)) rows.push(...treeRows(v, group.files, `${group.id}:`, 1));
  }
  return rows;
}

/** A tree's files in drawing order, closed folders included. */
function filesInOrder(files) {
  const all = [];
  const walk = dir => {
    for (const child of [...dir.dirs.values()].sort(byName)) walk(child);
    for (const { file } of [...dir.files].sort(byName)) all.push(file);
  };
  walk(buildTree(files));
  return all;
}

/** Every file in drawing order, closed folders included: what Alt+↑/↓ steps through. */
function visibleFiles(v) {
  if (v.sel !== WORK) return filesInOrder(v.files ?? []);
  return workGroups(v).flatMap(group => filesInOrder(group.files));
}

/** The files a tree row stands for: itself, everything under a folder, or a whole group. */
function rowFiles(row) {
  if (row.kind === 'file') return [row.file];
  if (row.kind === 'group') return row.group.files;
  const all = [];
  const walk = dir => {
    for (const child of dir.dirs.values()) walk(child);
    for (const { file } of dir.files) all.push(file);
  };
  walk(row.dir);
  return all;
}

/** Ticks a row's files, or unticks them when all are ticked already. */
function toggleTick(v, row) {
  const files = rowFiles(row);
  const all = files.every(f => v.ticked.has(f.path));
  for (const f of files) {
    if (all) v.ticked.delete(f.path);
    else v.ticked.add(f.path);
  }
  renderTree();
  updateComposer();
}

const FOLDER = '<svg class="folder icon" viewBox="0 0 16 16"><path d="M1.5 4.5v8h13v-7h-6.5l-1.5-1.5h-5z" /></svg>';
const CHEVRON = '<svg class="chev icon" viewBox="0 0 16 16"><path d="M4 6l4 4 4-4" /></svg>';

/** A file's type badge: its extension, coloured by language. */
const KINDS = {
  php: ['php', '#7b7fc4'],
  js: ['js', '#c9a227'],
  mjs: ['js', '#c9a227'],
  ts: ['ts', '#3a78c4'],
  tsx: ['tsx', '#3a78c4'],
  css: ['css', '#4a8fd6'],
  scss: ['css', '#c76394'],
  html: ['htm', '#d9714a'],
  tpl: ['tpl', '#d9714a'],
  json: ['{}', '#8c74c9'],
  md: ['md', '#6b7280'],
  sql: ['sql', '#c27c3e'],
  rs: ['rs', '#c76b3c'],
  toml: ['tml', '#6b7280'],
  yml: ['yml', '#b0477a'],
  yaml: ['yml', '#b0477a'],
  xml: ['xml', '#5f8f4e'],
  svg: ['svg', '#5f8f4e'],
  png: ['img', '#4f9a8f'],
  jpg: ['img', '#4f9a8f'],
  jpeg: ['img', '#4f9a8f'],
  gif: ['img', '#4f9a8f'],
  webp: ['img', '#4f9a8f'],
  bmp: ['img', '#4f9a8f'],
  ico: ['img', '#4f9a8f'],
  avif: ['img', '#4f9a8f'],
  sh: ['sh', '#5d6b78'],
};
function kindBadge(name) {
  const ext = name.includes('.') ? name.split('.').pop().toLowerCase() : '';
  const [label, colour] = KINDS[ext] ?? [ext.slice(0, 3) || '·', '#6b6f78'];
  const badge = el('span', 'kind', label);
  badge.style.background = colour;
  return badge;
}

const STATUS_WORD = { A: 'added', M: 'modified', D: 'deleted', R: 'renamed', C: 'copied', T: 'type changed', U: 'conflicted', '?': 'unversioned' };
/** A status letter as a class name: `?` is no valid one. */
const statusClass = status => (status === '?' ? 'Q' : status);

/** A row's tick box: ticked, unticked, or part-ticked for a folder or group. */
function tickBox(v, row) {
  const files = rowFiles(row);
  const ticked = files.filter(f => v.ticked.has(f.path)).length;
  const box = el('input', 'tick');
  box.type = 'checkbox';
  box.tabIndex = -1;
  box.checked = ticked > 0 && ticked === files.length;
  box.indeterminate = ticked > 0 && ticked < files.length;
  box.title = box.checked ? 'Leave out of the commit (Space)' : 'Include in the commit (Space)';
  box.setAttribute('aria-label', box.title);
  // The tree keeps the focus, so ↑/↓ and Space go on from the row.
  box.onmousedown = event => {
    event.preventDefault();
    $('tree').focus({ preventScroll: true });
  };
  box.onclick = event => {
    event.stopPropagation();
    v.cursor = row.key;
    toggleTick(v, row);
  };
  return box;
}

function renderTree() {
  const tree = $('tree');
  tree.replaceChildren();
  $('tree-sum').textContent = '';
  if (!current) return;
  const v = view(current);
  const work = v.sel === WORK;
  $('detail').classList.toggle('working', work);
  $('rollback').hidden = !work;
  if (!v.sel) return;
  if (v.filesError) {
    tree.append(el('div', 'empty', v.filesError));
    return;
  }
  if (v.filesSha !== v.sel) return;
  if (!v.files.length) {
    tree.append(el('div', 'empty', work ? 'No uncommitted changes. Edit, add or delete a file, and it shows here.' : 'This commit changes no files.'));
    return;
  }
  const ticked = work ? tickedFiles(v).length : 0;
  $('tree-sum').textContent = work ? `${ticked} of ${plural(v.files.length, 'file')} ticked` : plural(v.files.length, 'file');
  $('rollback').disabled = !ticked || Boolean(v.busy);
  for (const row of visibleRows(v)) {
    const node = el('div', `node ${row.kind}`);
    node.dataset.key = row.key;
    node.style.paddingLeft = `${10 + row.depth * 16}px`;
    if (row.kind === 'group') {
      if (v.closed.has(row.id)) node.classList.add('closed');
      node.innerHTML = CHEVRON;
      node.append(tickBox(v, row), el('span', 'name', row.group.label), el('span', 'count', plural(row.group.files.length, 'file')));
      node.onclick = () => {
        v.cursor = row.key;
        toggleDir(v, row.id);
      };
    } else if (row.kind === 'dir') {
      if (v.closed.has(row.id)) node.classList.add('closed');
      node.innerHTML = CHEVRON;
      if (work) node.append(tickBox(v, row));
      node.insertAdjacentHTML('beforeend', FOLDER);
      node.append(el('span', 'name', row.dir.name), el('span', 'count', plural(countOf(row.dir), 'file')));
      node.onclick = () => {
        v.cursor = row.key;
        toggleDir(v, row.id);
      };
    } else {
      const status = row.file.status.charAt(0);
      node.classList.add(statusClass(status));
      node.style.paddingLeft = `${29 + row.depth * 16}px`;
      node.title = `${row.file.path} (${STATUS_WORD[status] ?? row.file.status})`;
      if (work) node.append(tickBox(v, row));
      node.append(kindBadge(row.name), el('span', 'name', row.name));
      if (row.file.from) node.append(el('span', 'from', `from ${row.file.from}`));
      node.onclick = () => openFile(row.file.path);
    }
    if (row.key === v.cursor) node.classList.add('selected');
    tree.append(node);
  }
}

/** Opens or closes a folder or group by its `id`. */
function toggleDir(v, id, open) {
  const isOpen = !v.closed.has(id);
  if (open === undefined) open = !isOpen;
  if (open) v.closed.delete(id);
  else v.closed.add(id);
  renderTree();
}

function openFile(path, landing = null) {
  const v = view(current);
  v.file = path;
  v.cursor = `f:${path}`;
  v.diffOpen = true;
  renderTree();
  renderDiff(landing);
}

function renderMessage() {
  const box = $('message');
  const v = current ? view(current) : null;
  if (v?.sel === WORK) {
    // The commit box is built once and kept, so a read while typing leaves the text and caret alone.
    if (box.firstChild !== composer.root) box.replaceChildren(composer.root);
    updateComposer();
    return;
  }
  box.replaceChildren();
  if (!v) return;
  const c = selectedCommit(v);
  if (!c) return;
  const d = v.data;
  const subject = el('p', 'subject', c.subject);
  const where = d.commits.some(x => x.sha === c.sha)
    ? 'to push'
    : (d.incoming ?? []).some(x => x.sha === c.sha) ? `new on ${d.historyRef}` : `on ${d.historyRef}`;
  subject.append(el('span', 'where', where));
  box.append(subject);
  if (c.body) box.append(el('p', 'body', c.body));
  const meta = el('div', 'meta');
  const sha = el('span', 'sha', c.short);
  sha.title = `${c.sha} (click to copy)`;
  sha.onclick = () => navigator.clipboard?.writeText(c.sha).catch(() => {});
  meta.append(sha, ` ${c.author} `, el('span', 'email', `<${c.email}>`), ` on ${c.date}`);
  box.append(meta);
}

// ---------------------------------------------------------------- committing

/** The Uncommitted row's commit box: message, Amend, Commit and Commit and Push. */
const composer = buildComposer();

function buildComposer() {
  const root = el('div', 'composer');
  const text = el('textarea', 'commit-text');
  text.id = 'commit-text';
  text.placeholder = 'Commit message';
  text.spellcheck = true;
  text.setAttribute('aria-label', 'Commit message');
  const bar = el('div', 'composer-bar');
  const amendLabel = el('label', 'amend');
  const amend = el('input');
  amend.type = 'checkbox';
  amend.id = 'amend';
  amendLabel.append(amend, ' Amend');
  const note = el('span', 'composer-note');
  const commit = el('button', 'primary');
  commit.id = 'commit';
  const andPush = el('button');
  andPush.id = 'commit-push';
  bar.append(amendLabel, note, el('span', 'spacer'), andPush, commit);
  root.append(text, bar);

  text.addEventListener('input', () => {
    if (!current) return;
    view(current).draft = text.value;
    updateComposer();
  });
  text.addEventListener('keydown', event => {
    if (event.ctrlKey && event.key === 'Enter') {
      event.preventDefault();
      commitWork(event.shiftKey);
    }
  });
  amend.addEventListener('change', () => setAmend(amend.checked));
  commit.onclick = () => commitWork(false);
  andPush.onclick = () => commitWork(true);
  return { root, text, amend, amendLabel, note, commit, andPush };
}

/** Why the commit cannot be made now, or null. */
function commitBlocker(v) {
  if (v.busy) return `${current} is busy.`;
  if (v.work?.merging) return 'A merge is in progress; git commits it only whole. Finish it from a terminal.';
  if (!v.amend && !tickedFiles(v).length) return 'Tick the files to commit.';
  if (!v.draft.trim()) return 'Write a commit message.';
  return null;
}

/** Brings the commit box in line with the view: its text, Amend, the buttons and the note. */
function updateComposer() {
  if (!current) return;
  const v = view(current);
  if (v.sel !== WORK) return;
  const { text, amend, amendLabel, note, commit, andPush } = composer;
  const head = v.work?.head;
  // HEAD was pushed meanwhile (or there is none): Amend no longer applies.
  if (v.amend && (!head || head.pushed)) {
    v.amend = false;
    v.draft = v.amendSaved;
  }
  if (text.value !== v.draft) text.value = v.draft;
  amend.checked = v.amend;
  amend.disabled = !head || head.pushed || Boolean(v.busy);
  amendLabel.classList.toggle('off', amend.disabled);
  amendLabel.title = !head
    ? 'There is no commit to amend yet.'
    : head.pushed
      ? `${head.short} is pushed already; amending it would rewrite history others may have.`
      : `Add the ticked files to ${head.short} and edit its message, in place of a new commit.`;
  const ticked = tickedFiles(v).length;
  const blocker = commitBlocker(v);
  commit.disabled = Boolean(blocker);
  andPush.disabled = Boolean(blocker);
  commit.title = blocker ?? 'Ctrl+Enter';
  andPush.title = blocker ?? 'Commit, then push the branch (Ctrl+Shift+Enter)';
  const busy = v.busy === 'commit';
  commit.innerHTML = '';
  andPush.innerHTML = '';
  if (busy && composer.pushing) andPush.append(el('span', 'spinner'));
  else if (busy) commit.append(el('span', 'spinner'));
  commit.append(v.amend ? 'Amend Commit' : 'Commit');
  andPush.append(v.amend ? 'Amend and Push' : 'Commit and Push');
  if (v.work?.merging) note.textContent = 'A merge is in progress.';
  else if (v.amend) note.textContent = ticked ? `Amends ${head.short} with ${plural(ticked, 'file')}` : `Rewords ${head.short}`;
  else note.textContent = ticked ? plural(ticked, 'file') : '';
}

/** Amend puts HEAD's message in the box; unticking it brings back what was written before. */
function setAmend(on) {
  const v = view(current);
  const head = v.work?.head;
  if (on && head && !head.pushed) {
    v.amendSaved = v.draft;
    v.draft = head.message;
    v.amend = true;
  } else {
    if (v.amend && v.draft === head?.message) v.draft = v.amendSaved;
    v.amend = false;
  }
  updateComposer();
  composer.text.focus();
}

/** Why the branch cannot be pushed right now, or null. */
function pushBlocker(v) {
  const d = v.data;
  if (!d || d.error) return d?.error ?? 'the lists are not read yet';
  if (d.kept?.length) return 'finish or abandon the kept rebase first';
  if (d.behind > 0) return `rebase onto ${d.upstream} first`;
  if (!(d.ahead > 0)) return `${d.upstream} has it already`;
  return v.busy ? `${current} is busy` : null;
}

/** Commits the ticked files with the message (or amends HEAD), then pushes when `andPush`. */
async function commitWork(andPush) {
  const name = current;
  if (!name) return;
  const v = view(name);
  if (v.sel !== WORK || commitBlocker(v)) return;
  const amending = v.amend;
  v.busy = 'commit';
  composer.pushing = andPush;
  v.notice = null;
  progress(+1);
  renderAll();
  let made = null;
  try {
    made = await api.invoke('commit', { name, message: v.draft, files: tickedFiles(v), amend: amending });
    v.notice = { tone: 'success', text: `${amending ? 'Amended' : 'Committed'} ${made.short}: ${made.subject}` };
    v.draft = '';
    v.amendSaved = '';
    v.amend = false;
  } catch (error) {
    v.notice = { tone: 'error', text: String(error) };
  }
  v.busy = null;
  progress(-1);
  await read(name, false);
  if (!made || !andPush) return;
  const blocker = pushBlocker(v);
  if (blocker) {
    v.notice = { tone: 'warning', text: `${amending ? 'Amended' : 'Committed'} ${made.short}, but did not push: ${blocker}.` };
    if (name === current) renderAlerts();
    return;
  }
  if (name === current) await push();
}

/** Rolls `files` back to HEAD after asking; unversioned files among them are deleted. */
async function rollbackFiles(files) {
  const name = current;
  if (!name || !files.length) return;
  const v = view(name);
  if (v.busy) return;
  const gone = files.filter(f => f.status === '?').length;
  const back = files.length - gone;
  const added = files.some(f => ['A', 'C', 'R'].includes(f.status));
  const lines = [];
  if (back) lines.push(`${back === 1 ? 'Its' : 'Their'} changes are undone, back to HEAD.`);
  if (added) lines.push('A file added to git stays on disk, unversioned.');
  if (gone) lines.push(`${plural(gone, 'unversioned file')} ${gone === 1 ? 'is' : 'are'} deleted from disk, for good.`);
  const only = files.length === 1 ? files[0].path.split('/').pop() : plural(files.length, 'file');
  await ask({
    title: gone === files.length ? `Delete ${only}?` : `Roll back ${only}?`,
    text: lines.join(' '),
    detail: files.map(f => `${(STATUS_WORD[f.status] ?? f.status).padEnd(12)} ${f.path}`).join('\n'),
    ok: gone === files.length ? 'Delete' : 'Roll Back',
    danger: true,
    run: async () => {
      v.busy = 'rollback';
      progress(+1);
      let notice;
      try {
        await api.invoke('rollback', { name, files });
        notice = { tone: 'success', text: gone === files.length ? `Deleted ${only}.` : `Rolled back ${only}.` };
      } catch (error) {
        notice = { tone: 'error', text: String(error) };
      }
      v.busy = null;
      progress(-1);
      return notice;
    },
  });
}

/** The tree's right-click menu for the Uncommitted row: tick, diff, roll back, copy the path. */
function workMenu(v, row) {
  const files = rowFiles(row);
  const all = files.every(f => f.status === '?');
  const ticked = files.every(f => v.ticked.has(f.path));
  return [
    ...(row.kind === 'file' ? [{ label: 'Show Diff', key: 'Enter', run: () => openFile(row.file.path) }] : []),
    { label: ticked ? 'Leave Out of the Commit' : 'Include in the Commit', key: 'Space', run: () => toggleTick(v, row) },
    { sep: true },
    { label: all ? 'Delete…' : 'Rollback…', key: 'Ctrl+Alt+Z', danger: true, disabled: Boolean(v.busy), run: () => rollbackFiles(files) },
    { sep: true },
    {
      label: 'Copy Path',
      run: () => copyText(row.kind === 'file' ? row.file.path : row.kind === 'dir' ? row.dir.path : files.map(f => f.path).join('\n'), 'Copied.'),
    },
  ];
}

/** The docked panel's diff; null until the page starts. */
let docked = null;
/** The person's choice: diffs open in their own window instead of the panel. Kept for the next start. */
let undocked = false;
/** The diff window is open (or opening). */
let windowUp = false;
/** Where F7's place goes once a window that is still loading asks for its file. */
let windowLanding = null;
/** What the diff window was last told to show, sent again when it (re)loads. */
let sentToWindow = null;

/** The open file and its commit, as a diff view takes them: the files in the tree's order. */
function diffTarget(v) {
  if (v.sel === WORK) return { name: current, sha: WORK, short: 'uncommitted', files: visibleFiles(v), path: v.file, stamp: v.workStamp };
  return { name: current, sha: v.sel, short: selectedCommit(v)?.short, files: visibleFiles(v), path: v.file };
}

/** What the diff window was sent, to skip sending the same again: an uncommitted file once per read. */
const windowKey = (target, path = target.path) => `${target.name}\n${target.sha}\n${path}\n${target.files.length}\n${target.stamp ?? 0}`;

/**
 * Draws the open file's diff in the panel, or hands it to the diff window when undocked.
 * `landing` ('first' or 'last') puts F7's place on that change of the file.
 */
async function renderDiff(landing = null) {
  const work = $('work');
  const v = current ? view(current) : null;
  const file = v && v.diffOpen ? v.files?.find(f => f.path === v.file) : null;
  const open = Boolean(file && v.filesSha === v.sel);
  const inPanel = open && !undocked;
  $('diff').hidden = !inPanel;
  $('diff-gutter').hidden = !inPanel;
  work.classList.toggle('with-diff', inPanel);
  if (undocked) {
    // The window asks for its file once it has loaded (`diff-ready`).
    if (open && !windowUp) openDiffWindow(landing);
    else if (windowUp) sendToWindow(open ? diffTarget(v) : null, landing);
    return;
  }
  if (!open) {
    docked.reset();
    return;
  }
  $('view-split').classList.toggle('on', layout === 'split');
  $('view-unified').classList.toggle('on', layout === 'unified');
  await docked.show(diffTarget(v), landing);
}

function sendToWindow(target, landing = null) {
  const key = target ? windowKey(target) : '';
  if (!landing && sentToWindow?.key === key) return;
  sentToWindow = { key, target };
  api.emitTo('diff', 'diff-show', { target, landing }).catch(() => {});
}

async function openDiffWindow(landing) {
  windowUp = true;
  windowLanding = landing;
  sentToWindow = null;
  try {
    await api.invoke('diff_window');
  } catch (error) {
    windowUp = false;
    setUndocked(false);
    renderDiff();
    settle({ tone: 'error', text: `Could not open the diff window: ${error}` });
  }
}

function setUndocked(next) {
  undocked = next;
  saved.set('undocked', next);
}

/** Moves the diff into its own window, and every later diff with it: the panel closes. */
function undock() {
  if (!current || undocked) return;
  setUndocked(true);
  docked.reset();
  renderDiff();
}

/** Back into the panel, and every later diff with it; the diff window closes. */
function dock() {
  if (!undocked) return;
  setUndocked(false);
  closeDiffWindow();
  renderDiff();
}

/** Closes the diff window; marked closed first, so its `diff-window-closed` closes no diff. */
function closeDiffWindow() {
  if (!windowUp) return;
  windowUp = false;
  api.invoke('close_diff_window').catch(() => {});
}

/** F7 (1) or Shift+F7 (-1): the next or previous change, across the commit's files. */
function stepChange(dir) {
  const v = view(current);
  if (windowUp && v.diffOpen) {
    api.emitTo('diff', 'diff-step', dir).catch(() => {});
    return;
  }
  if (!v.diffOpen || !v.file) {
    const files = visibleFiles(v);
    const first = dir > 0 ? files[0] : files[files.length - 1];
    if (first) openFile(first.path, dir > 0 ? 'first' : 'last');
    return;
  }
  docked.step(dir);
}

/** F7 crossed into another file of the commit: the tree follows it. */
function followDiff(path, landing) {
  openFile(path, landing);
  $('tree').querySelector('.node.selected')?.scrollIntoView({ block: 'nearest' });
}

function renderFooter() {
  const button = $('push');
  const note = $('foot-note');
  const cancelButton = $('cancel');
  note.replaceChildren();
  const v = current ? view(current) : null;
  const d = v?.data;
  button.disabled = !v || !canPush(v);
  cancelButton.textContent = d && !d.error && d.ahead > 0 ? 'Cancel' : 'Close';
  if (v?.busy === 'push') {
    button.innerHTML = '<span class="spinner"></span>Pushing…';
    return;
  }
  button.textContent = d && d.ahead > 0 ? `Push ${plural(d.ahead, 'commit')}` : 'Push';
  if (!d || d.error) return;
  if (d.kept?.length) {
    note.textContent = 'Finish or abandon the kept rebase first.';
  } else if (d.behind > 0) {
    note.textContent = `Rebase onto ${d.upstream} first.`;
  } else if (d.ahead > 0) {
    note.append('Sends ', el('span', 'mono', d.head.slice(0, 10)), ` to ${d.upstream}, and nothing newer.`);
  } else {
    note.textContent = `Up to date with ${d.upstream}.`;
  }
}

// ---------------------------------------------------------------- branches

let showBranches = true;

async function readBranches(name) {
  const v = view(name);
  const token = ++v.branchesToken;
  try {
    const branches = await api.invoke('branches', { name });
    if (token !== v.branchesToken) return;
    v.branches = branches;
    v.branchesError = null;
  } catch (error) {
    if (token !== v.branchesToken) return;
    v.branchesError = String(error);
  }
  if (name === current) renderBranches();
}

/** Branches as nested folders by their `/` prefixes. */
function branchTree(list) {
  const root = { path: '', dirs: new Map(), items: [] };
  for (const b of list) {
    const parts = b.name.split('/');
    const leaf = parts.pop();
    let node = root;
    for (const part of parts) {
      const path = node.path ? `${node.path}/${part}` : part;
      if (!node.dirs.has(part)) node.dirs.set(part, { name: part, path, dirs: new Map(), items: [] });
      node = node.dirs.get(part);
    }
    node.items.push({ b, leaf });
  }
  return root;
}

const branchCount = dir => dir.items.length + [...dir.dirs.values()].reduce((n, d) => n + branchCount(d), 0);
const byLeaf = (a, b) => a.leaf.localeCompare(b.leaf, undefined, { sensitivity: 'base' });
const branchKey = b => (b.remote ? `b:r:${b.remote}/${b.name}` : `b:l:${b.name}`);

/**
 * The column's rows in drawing order. Groups and remotes start open, folders closed unless they
 * hold the current branch; `branchOpen` keeps what the person opened or closed. The default branch
 * (`main`) is pinned first, then folders, then the other branches.
 */
function branchRows(v) {
  const found = v.branches;
  if (!found) return [];
  const rows = [];
  const open = (key, byDefault) => v.branchOpen.get(key) ?? byDefault;
  const defaults = new Set(found.remote.filter(b => b.default).map(b => b.name));
  const walk = (dir, scope, depth, pinned) => {
    const items = [...dir.items].sort(byLeaf);
    for (const item of items.filter(i => pinned(i.b))) rows.push({ kind: 'branch', key: branchKey(item.b), depth, ...item });
    for (const child of [...dir.dirs.values()].sort(byName)) {
      const key = `d:${scope}:${child.path}`;
      const holdsHead = scope === 'l' && found.head.startsWith(`${child.path}/`);
      const isOpen = open(key, holdsHead);
      rows.push({ kind: 'dir', key, depth, dir: child, open: isOpen });
      if (isOpen) walk(child, scope, depth + 1, () => false);
    }
    for (const item of items.filter(i => !pinned(i.b))) rows.push({ kind: 'branch', key: branchKey(item.b), depth, ...item });
  };

  const localOpen = open('g:local', true);
  rows.push({ kind: 'group', key: 'g:local', depth: 0, label: 'Local', count: found.local.length, open: localOpen });
  if (localOpen) walk(branchTree(found.local), 'l', 1, b => defaults.has(b.name));

  const remotes = [...new Set(found.remote.map(b => b.remote))];
  const remoteOpen = open('g:remote', true);
  rows.push({ kind: 'group', key: 'g:remote', depth: 0, label: 'Remote', count: found.remote.length, open: remoteOpen });
  if (remoteOpen) {
    for (const remote of remotes) {
      const key = `r:${remote}`;
      const list = found.remote.filter(b => b.remote === remote);
      const isOpen = open(key, true);
      rows.push({ kind: 'remote', key, depth: 1, label: remote, count: list.length, open: isOpen });
      if (isOpen) walk(branchTree(list), key, 2, b => b.default);
    }
  }
  return rows;
}

const BRANCH_ICON =
  '<svg class="icon branch-icon" viewBox="0 0 16 16"><circle cx="5" cy="4" r="1.6" /><circle cx="5" cy="12" r="1.6" /><circle cx="11" cy="6" r="1.6" /><path d="M5 5.6v4.8 M11 7.6c0 2.2-2.6 2.4-5.4 3.4" /></svg>';
const REMOTE_ICON = '<svg class="icon branch-icon" viewBox="0 0 16 16"><path d="M4.5 12.5a3 3 0 0 1-.4-6 4 4 0 0 1 7.8-.6 3 3 0 0 1 .1 6.6z" /></svg>';

/** ↑2 ↓1, gone and worktree marks after a branch's name. */
function branchMarks(b) {
  const marks = el('span', 'marks');
  if (b.ahead) marks.append(el('span', 'ahead', `↑${b.ahead}`));
  if (b.behind) marks.append(el('span', 'behind', `↓${b.behind}`));
  if (b.gone) marks.append(el('span', 'gone', 'gone'));
  if (b.worktree) marks.append(el('span', 'wt', 'worktree'));
  return marks;
}

/** "5 Nov 2024, 14:03", for hovering an age: old branches need the year. */
function fullDate(at) {
  return new Date(at).toLocaleString('en-GB', { day: 'numeric', month: 'short', year: 'numeric', hour: '2-digit', minute: '2-digit' });
}

function branchTitle(b) {
  const full = b.remote ? `${b.remote}/${b.name}` : b.name;
  const lines = [full];
  if (b.current) lines.push('Checked out here');
  if (b.worktree) lines.push(`Checked out in ${b.worktree}`);
  if (!b.remote) lines.push(b.upstream ? `Tracks ${b.upstream}${b.gone ? ' (gone on the remote)' : ''}` : 'No upstream');
  if (b.ahead || b.behind) lines.push(`${b.ahead} ahead, ${b.behind} behind`);
  lines.push(`${b.sha} ${b.subject}`, `Last commit ${fullDate(b.at)}`);
  return lines.join('\n');
}

function renderBranches() {
  const columns = $('columns');
  columns.classList.toggle('no-branches', !showBranches);
  $('branches').hidden = !showBranches;
  $('branches-gutter').hidden = !showBranches;
  $('toggle-branches').setAttribute('aria-pressed', String(showBranches));
  $('toggle-branches').classList.toggle('on', showBranches);
  if (!showBranches) return;
  const tree = $('branch-tree');
  const keep = tree.scrollTop;
  tree.replaceChildren();
  if (!current) return;
  const v = view(current);
  $('new-branch').disabled = $('fetch-all').disabled = Boolean(v.busy);
  if (v.branchesError) {
    tree.append(el('div', 'empty', v.branchesError));
    return;
  }
  const found = v.branches;
  if (!found) return;

  const head = el('div', 'branch-head');
  head.append(el('span', 'label', 'HEAD'));
  const now = found.local.find(b => b.current);
  if (now) {
    const name = el('span', 'name');
    name.innerHTML = BRANCH_ICON;
    name.append(el('span', '', now.name));
    head.append(name, branchMarks(now));
    head.title = branchTitle(now);
  } else {
    head.append(el('span', 'name detached', found.detached ? `detached at ${found.detached}` : 'no commits yet'));
  }
  tree.append(head);

  for (const row of branchRows(v)) {
    const node = el('div', `node bnode ${row.kind}`);
    node.dataset.key = row.key;
    node.style.paddingLeft = `${8 + row.depth * 14}px`;
    if (row.kind === 'branch') {
      const b = row.b;
      node.style.paddingLeft = `${27 + row.depth * 14}px`;
      if (b.current) node.classList.add('current');
      if (b.gone) node.classList.add('is-gone');
      node.innerHTML = b.default ? '<span class="star" title="The remote\'s default branch">★</span>' : BRANCH_ICON;
      node.append(el('span', 'name', row.leaf), branchMarks(b));
      node.title = branchTitle(b);
      node.onclick = () => pickBranchRow(row.key);
      node.ondblclick = () => switchBranch(b);
      node.oncontextmenu = event => {
        event.preventDefault();
        pickBranchRow(row.key);
        openMenu(event.clientX, event.clientY, branchMenu(b));
      };
    } else {
      if (!row.open) node.classList.add('closed');
      const label = row.kind === 'dir' ? row.dir.name : row.label;
      const count = row.kind === 'dir' ? branchCount(row.dir) : row.count;
      node.innerHTML = CHEVRON + (row.kind === 'dir' ? FOLDER : row.kind === 'remote' ? REMOTE_ICON : '');
      node.append(el('span', 'name', label), el('span', 'count', String(count)));
      node.onclick = () => {
        pickBranchRow(row.key);
        toggleBranchNode(v, row);
      };
      node.oncontextmenu = event => {
        event.preventDefault();
        if (!isLocalFolder(row)) return;
        pickBranchRow(row.key);
        openMenu(event.clientX, event.clientY, folderMenu(row.dir));
      };
    }
    if (row.key === v.branchCursor) node.classList.add('selected');
    tree.append(node);
  }
  tree.scrollTop = keep;
}

function pickBranchRow(key) {
  view(current).branchCursor = key;
  for (const node of $('branch-tree').querySelectorAll('.bnode')) node.classList.toggle('selected', node.dataset.key === key);
  $('branch-tree').focus({ preventScroll: true });
}

function toggleBranchNode(v, row, wantOpen) {
  if (wantOpen !== undefined && wantOpen === row.open) return;
  v.branchOpen.set(row.key, !row.open);
  renderBranches();
}

/** The picked branch, when the cursor is on one. */
function pickedBranch(v) {
  return branchRows(v).find(r => r.key === v.branchCursor && r.kind === 'branch')?.b ?? null;
}

/** A local branch's upstream as a remote branch still listed, or null. */
function upstreamOf(v, b) {
  if (b.remote || !b.upstream || b.gone) return null;
  return v.branches?.remote.find(r => `${r.remote}/${r.name}` === b.upstream) ?? null;
}

function branchMenu(b) {
  const v = view(current);
  const full = b.remote ? `${b.remote}/${b.name}` : b.name;
  const items = [
    { label: b.current ? 'Checked out' : b.remote ? `Switch to ${b.name}` : 'Switch to', run: () => switchBranch(b), disabled: b.current },
    { label: `New branch from ${full}…`, run: () => newBranch(full) },
  ];
  if (!b.remote) {
    items.push({ label: 'Rename…', run: () => renameBranch(b), key: 'F2' });
    items.push({ sep: true });
    items.push({ label: 'Delete', run: () => deleteBranch(b), disabled: b.current, danger: true, key: 'Del' });
    const up = upstreamOf(v, b);
    if (up) items.push({ label: `Delete on ${up.remote}…`, run: () => deleteRemoteBranch(up), disabled: up.default, danger: true });
  } else {
    items.push({ sep: true });
    items.push({ label: `Delete on ${b.remote}…`, run: () => deleteRemoteBranch(b), disabled: b.default, danger: true, key: 'Del' });
  }
  items.push({ sep: true });
  items.push({ label: 'Copy name', run: () => copyText(full, `Copied ${full}.`) });
  return items;
}

/** A folder of local branches: what can be renamed or deleted as one. */
const isLocalFolder = row => row?.kind === 'dir' && row.key.startsWith('d:l:');

/** The local branches under a folder, nested folders included. */
function folderBranches(dir) {
  return [...dir.items.map(i => i.b), ...[...dir.dirs.values()].flatMap(folderBranches)];
}

function folderMenu(dir) {
  return [
    { label: 'New branch in folder…', run: () => newBranch('HEAD', `${dir.path}/`) },
    { label: 'Rename folder…', run: () => renameFolder(dir), key: 'F2' },
    { sep: true },
    { label: 'Delete folder…', run: () => deleteFolder(dir), danger: true, key: 'Del' },
    { sep: true },
    { label: 'Copy name', run: () => copyText(dir.path, `Copied ${dir.path}.`) },
  ];
}

async function renameFolder(dir) {
  const v = view(current);
  const count = branchCount(dir);
  await ask({
    title: `Rename folder ${dir.path}`,
    text: `Renames the ${branchesWord(count)} under ${dir.path}/. Their upstreams stay as they are.`,
    input: dir.path,
    ok: 'Rename',
    run: async to => {
      const next = to.trim().replace(/^\/+|\/+$/g, '');
      if (next === dir.path) return null;
      const notice = await branchStep('branch_rename_folder', { from: dir.path, to: next }, 'folder rename');
      // The renamed folder stays open or closed as it was.
      if (notice.tone === 'success' && v.branchOpen.has(`d:l:${dir.path}`)) v.branchOpen.set(`d:l:${next}`, v.branchOpen.get(`d:l:${dir.path}`));
      return notice;
    },
  });
}

/**
 * Deletes every local branch under a folder after a yes. Branches whose commits are on no other
 * branch are kept by the first step and asked about again, as a single branch's delete does.
 */
async function deleteFolder(dir) {
  const name = current;
  const v = view(name);
  const list = folderBranches(dir);
  const head = list.find(b => b.current);
  const shown = list.slice(0, 40).map(b => b.name);
  if (list.length > shown.length) shown.push(`… and ${list.length - shown.length} more`);
  let first = null;
  const sure = await ask({
    title: `Delete folder ${dir.path}?`,
    text: `Deletes the ${branchesWord(list.length)} under ${dir.path}/. Branches with commits that are on no other branch are kept until you say so${head ? `, and ${head.name} stays: it is checked out` : ''}. Remote branches are not touched.`,
    detail: shown.join('\n'),
    ok: `Delete ${branchesWord(list.length)}`,
    danger: true,
    run: async () => {
      const done = await branchStep('branch_delete_folder', { folder: dir.path, force: false, branches: null }, 'folder delete');
      if (!done.notice) return done;
      first = done;
      return done.notice.tone === 'error' ? done.notice : null;
    },
  });
  if (!sure || !first) return;
  if (!first.unmerged.length) {
    settle(first.notice);
    return;
  }
  // The merged ones are gone already: the tree shows what is left while the second ask is up.
  v.notice = first.notice;
  renderAlerts();
  read(name, false);
  const forced = await ask({
    title: first.unmerged.length === 1 ? `Delete ${first.unmerged[0]}? It is not merged.` : `Delete ${first.unmerged.length} branches that are not merged?`,
    text: first.notice.text,
    detail: first.notice.detail,
    ok: 'Delete anyway',
    danger: true,
    run: async () => {
      const done = await branchStep('branch_delete_folder', { folder: dir.path, force: true, branches: first.unmerged }, 'folder delete');
      return done.notice ?? done;
    },
  });
  if (!forced && view(name).notice === first.notice) {
    view(name).notice = { ...first.notice, tone: 'success', text: first.notice.text.replace(/ Delete (it|them) anyway\?$/, first.unmerged.length === 1 ? ' It was kept.' : ' They were kept.') };
    renderAlerts();
  }
}

const branchesWord = n => (n === 1 ? '1 branch' : `${n} branches`);

async function copyText(text, done) {
  const v = view(current);
  try {
    await navigator.clipboard.writeText(text);
    v.notice = { tone: 'success', text: done };
  } catch {
    v.notice = { tone: 'warning', text: 'Could not reach the clipboard.', detail: text };
  }
  renderAlerts();
}

/** Runs one branch command on the current repo; the answer is a notice. */
async function branchStep(cmd, args, busy) {
  const name = current;
  const v = view(name);
  if (v.busy) return { tone: 'error', text: `${name} is busy; try again in a moment.` };
  v.busy = busy;
  v.notice = null;
  progress(+1);
  renderAll();
  let notice;
  try {
    notice = await api.invoke(cmd, { name, ...args });
  } catch (error) {
    notice = { tone: 'error', text: String(error) };
  }
  v.busy = null;
  progress(-1);
  renderAll();
  return notice;
}

/** Shows a step's notice and reads the repo again when it changed something. */
function settle(notice) {
  const name = current;
  view(name).notice = notice;
  renderAlerts();
  if (notice.tone === 'success') read(name, false);
}

async function switchBranch(b) {
  if (b.current) return;
  settle(await branchStep('branch_switch', { branch: b.name, remote: b.remote }, 'branch switch'));
}

async function newBranch(base, prefix = '') {
  const from = base === 'HEAD' ? (view(current).branches?.head || 'HEAD') : base;
  await ask({
    title: 'New branch',
    text: `From ${from}. It tracks nothing until it is pushed.`,
    input: prefix,
    placeholder: 'feat/short-name',
    check: { label: 'Switch to it', checked: true },
    ok: 'Create',
    run: (branch, swap) => branchStep('branch_create', { branch: branch.trim(), base, switch: swap }, 'new branch'),
  });
}

async function renameBranch(b) {
  if (b.remote) return;
  await ask({
    title: `Rename ${b.name}`,
    input: b.name,
    ok: 'Rename',
    run: to => (to.trim() === b.name ? null : branchStep('branch_rename', { from: b.name, to: to.trim() }, 'branch rename')),
  });
}

async function deleteBranch(b) {
  if (b.remote) return deleteRemoteBranch(b);
  if (b.current) {
    settle({ tone: 'error', text: `${b.name} is checked out; switch to another branch before deleting it.` });
    return;
  }
  const first = await branchStep('branch_delete', { branch: b.name, force: false }, 'branch delete');
  if (first.tone !== 'warning') {
    settle(first);
    return;
  }
  // Commits that are on no other branch: ask before they go.
  await ask({
    title: `Delete ${b.name}?`,
    text: first.text,
    detail: first.detail,
    ok: 'Delete anyway',
    danger: true,
    run: () => branchStep('branch_delete', { branch: b.name, force: true }, 'branch delete'),
  });
}

async function deleteRemoteBranch(b) {
  if (b.default) return;
  await ask({
    title: `Delete ${b.name} on ${b.remote}?`,
    text: `This removes ${b.remote}/${b.name} for everyone who fetches from ${b.remote}. Local branches keep their commits.`,
    ok: `Delete on ${b.remote}`,
    danger: true,
    run: () => branchStep('branch_delete_remote', { remote: b.remote, branch: b.name }, 'remote branch delete'),
  });
}

async function fetchAll() {
  const notice = await branchStep('branch_fetch', {}, 'fetch');
  if (notice.tone === 'success') {
    view(current).fetchedAt = Date.now();
    read(current, false);
  } else {
    settle(notice);
  }
}

function toggleBranches() {
  showBranches = !showBranches;
  saved.set('branches', showBranches);
  renderBranches();
  if (showBranches) $('branch-tree').focus();
}

function moveBranchCursor(step) {
  const v = view(current);
  const rows = branchRows(v);
  if (!rows.length) return;
  const at = rows.findIndex(r => r.key === v.branchCursor);
  const next = rows[at < 0 ? 0 : Math.min(rows.length - 1, Math.max(0, at + step))];
  pickBranchRow(next.key);
  $('branch-tree').querySelector('.bnode.selected')?.scrollIntoView({ block: 'nearest' });
}

function branchTreeKey(event) {
  const v = view(current);
  const row = branchRows(v).find(r => r.key === v.branchCursor);
  const b = row?.kind === 'branch' ? row.b : null;
  if (event.key === 'ArrowDown') moveBranchCursor(1);
  else if (event.key === 'ArrowUp') moveBranchCursor(-1);
  else if (event.key === 'Enter' && b) switchBranch(b);
  else if (event.key === 'Enter' && row) toggleBranchNode(v, row);
  else if (event.key === 'ArrowLeft' && row && row.kind !== 'branch') toggleBranchNode(v, row, false);
  else if (event.key === 'ArrowRight' && row && row.kind !== 'branch') toggleBranchNode(v, row, true);
  else if (event.key === 'F2' && b && !b.remote) renameBranch(b);
  else if (event.key === 'F2' && isLocalFolder(row)) renameFolder(row.dir);
  else if (event.key === 'Delete' && b) deleteBranch(b);
  else if (event.key === 'Delete' && isLocalFolder(row)) deleteFolder(row.dir);
  else if ((event.key === 'ContextMenu' || (event.shiftKey && event.key === 'F10')) && (b || isLocalFolder(row))) {
    const box = $('branch-tree').querySelector('.bnode.selected')?.getBoundingClientRect();
    if (box) openMenu(box.left + 24, box.bottom, b ? branchMenu(b) : folderMenu(row.dir));
  } else return false;
  return true;
}

// ---------------------------------------------------------------- menu and dialog

const CHECK = '<svg class="icon" viewBox="0 0 16 16"><path d="M3 8.5l3 3 7-7" /></svg>';

/** Opens the menu at (x, y); closing it puts the focus back on `back`. */
function openMenu(x, y, items, back = $('branch-tree')) {
  menuBack = back;
  $('repo').setAttribute('aria-expanded', String(back === $('repo')));
  const menu = $('menu');
  menu.replaceChildren();
  // A menu with a ticked item (the repo shown) keeps a check column on every item, so labels line up.
  const marked = items.some(item => 'checked' in item);
  for (const item of items) {
    if (item.sep) {
      menu.append(el('div', 'menu-sep'));
      continue;
    }
    const button = el('button', item.danger ? 'danger' : '');
    button.type = 'button';
    button.setAttribute('role', 'checked' in item ? 'menuitemradio' : 'menuitem');
    if ('checked' in item) button.setAttribute('aria-checked', String(item.checked));
    if (item.title) button.title = item.title;
    if (marked) button.append(Object.assign(el('span', 'check'), { innerHTML: item.checked ? CHECK : '' }));
    button.append(el('span', 'label', item.label));
    if (item.key) button.append(el('span', 'key', item.key));
    button.disabled = Boolean(item.disabled);
    button.onclick = () => {
      closeMenu();
      item.run();
    };
    menu.append(button);
  }
  menu.hidden = false;
  // Keep it inside the window.
  const box = menu.getBoundingClientRect();
  menu.style.left = `${Math.min(x, window.innerWidth - box.width - 6)}px`;
  menu.style.top = `${Math.min(y, window.innerHeight - box.height - 6)}px`;
  (menu.querySelector('[aria-checked="true"]') ?? menu.querySelector('button:not(:disabled)'))?.focus();
}

/** Where the focus goes back to when the menu closes. */
let menuBack = null;

function closeMenu() {
  if ($('menu').hidden) return;
  $('menu').hidden = true;
  $('repo').setAttribute('aria-expanded', 'false');
  (menuBack ?? $('branch-tree')).focus({ preventScroll: true });
}

function menuKey(event) {
  const buttons = [...$('menu').querySelectorAll('button:not(:disabled)')];
  const at = buttons.indexOf(document.activeElement);
  if (event.key === 'ArrowDown') buttons[(at + 1) % buttons.length]?.focus();
  else if (event.key === 'ArrowUp') buttons[(at - 1 + buttons.length) % buttons.length]?.focus();
  else if (event.key === 'Escape') closeMenu();
  else return;
  event.preventDefault();
}

let modalDone = null;

/**
 * A small in-page dialog: a text, an optional name input and check box. `run(value, checked)`
 * does the step and answers a notice: an error keeps the dialog open with git's say, anything
 * else closes it and lands in the alert strip. Resolves true once the step ran, false on cancel.
 */
function ask({ title, text = '', input, placeholder = '', check, detail = '', ok, danger = false, run }) {
  closeMenu();
  $('modal-title').textContent = title;
  $('modal-text').textContent = text;
  $('modal-text').hidden = !text;
  const field = $('modal-input');
  field.hidden = input === undefined;
  field.value = input ?? '';
  field.placeholder = placeholder;
  $('modal-check').hidden = !check;
  $('modal-checkbox').checked = Boolean(check?.checked);
  $('modal-check-label').textContent = check?.label ?? '';
  $('modal-detail').textContent = detail;
  $('modal-detail').hidden = !detail;
  $('modal-error').hidden = true;
  const okButton = $('modal-ok');
  okButton.textContent = ok;
  okButton.classList.toggle('danger', danger);
  okButton.disabled = false;
  $('modal').hidden = false;
  if (input !== undefined) {
    field.focus();
    field.select();
  } else {
    okButton.focus();
  }

  return new Promise(resolve => {
    const close = (ran = false) => {
      $('modal').hidden = true;
      modalDone = null;
      $('modal-form').onsubmit = null;
      resolve(ran);
      $('branch-tree').focus({ preventScroll: true });
    };
    modalDone = close;
    $('modal-form').onsubmit = async event => {
      event.preventDefault();
      if (okButton.disabled) return;
      if (input !== undefined && !field.value.trim()) {
        field.focus();
        return;
      }
      okButton.disabled = true;
      okButton.replaceChildren(el('span', 'spinner'), ok);
      const notice = await run(field.value, $('modal-checkbox').checked);
      okButton.textContent = ok;
      okButton.disabled = false;
      if (notice && notice.tone === 'error') {
        const error = $('modal-error');
        error.textContent = notice.detail ? `${notice.text}\n${notice.detail}` : notice.text;
        error.hidden = false;
        return;
      }
      close(true);
      if (notice) settle(notice);
    };
  });
}

// ---------------------------------------------------------------- updates

/** The last check: the running version and the newer release's version and notes, if one is out. */
let update = { current: '', available: null, notes: '' };
/** A notice about updates; it shows whichever repo is open, so it lives outside the views. */
let updateNotice = null;

/** Asks GitHub for the latest release. A quiet check (on start, every few hours) only marks the button. */
async function checkUpdate(quiet) {
  try {
    update = await api.invoke('update_check');
  } catch (error) {
    if (!quiet) noteUpdate({ tone: 'warning', text: 'Checking for a Drop In update failed.', detail: String(error) });
    return false;
  }
  const button = $('update');
  button.classList.toggle('available', Boolean(update.available));
  button.title = update.available
    ? `Drop In ${update.available} is out (you run ${update.current}): update`
    : `Drop In ${update.current}: check for updates`;
  return true;
}

function noteUpdate(notice) {
  updateNotice = notice;
  renderAlerts();
}

/** The update button: checks now, and offers the newer release when there is one. */
async function onUpdate() {
  const button = $('update');
  button.disabled = true;
  const checked = await checkUpdate(false);
  button.disabled = false;
  if (!checked) return;
  if (!update.available) {
    noteUpdate({ tone: 'success', text: `Drop In ${update.current} is the latest version.` });
    return;
  }
  noteUpdate(null);
  ask({
    title: `Update to Drop In ${update.available}?`,
    text: `You run ${update.current}. Drop In downloads the new version, installs it and starts again.`,
    detail: update.notes,
    ok: 'Update and restart',
    run: async () => {
      try {
        // On success the app exits for the installer, so this call does not come back.
        await api.invoke('update_install');
        return null;
      } catch (error) {
        return { tone: 'error', text: 'The update did not install.', detail: String(error) };
      }
    },
  });
}

// ---------------------------------------------------------------- keys and wiring

function moveCommit(step) {
  const v = view(current);
  const all = rowOrder(v);
  const at = all.indexOf(v.sel);
  const next = all[Math.min(all.length - 1, Math.max(0, at + step))];
  if (next) pickCommit(next);
}

function moveCursor(step) {
  const v = view(current);
  const rows = visibleRows(v);
  if (!rows.length) return;
  const at = rows.findIndex(r => r.key === v.cursor);
  const next = rows[at < 0 ? 0 : Math.min(rows.length - 1, Math.max(0, at + step))];
  v.cursor = next.key;
  if (next.kind === 'file' && v.diffOpen) {
    openFile(next.file.path);
  } else {
    renderTree();
  }
  $('tree').querySelector('.node.selected')?.scrollIntoView({ block: 'nearest' });
}

function stepFile(step) {
  const v = view(current);
  const files = visibleFiles(v);
  if (!files.length) return;
  const at = files.findIndex(f => f.path === v.file);
  const next = files[at < 0 ? 0 : (at + step + files.length) % files.length];
  openFile(next.path);
  $('tree').querySelector('.node.selected')?.scrollIntoView({ block: 'nearest' });
}

function treeKey(event) {
  const v = view(current);
  const row = visibleRows(v).find(r => r.key === v.cursor);
  if (event.key === 'ArrowDown') moveCursor(1);
  else if (event.key === 'ArrowUp') moveCursor(-1);
  else if (event.key === 'Enter' && row) {
    if (row.kind === 'file') openFile(row.file.path);
    else toggleDir(v, row.id);
  } else if (event.key === 'ArrowLeft' && row && row.kind !== 'file') toggleDir(v, row.id, false);
  else if (event.key === 'ArrowRight' && row && row.kind !== 'file') toggleDir(v, row.id, true);
  else if (event.key === ' ' && row && v.sel === WORK) toggleTick(v, row);
  else if ((event.key === 'ContextMenu' || (event.shiftKey && event.key === 'F10')) && row && v.sel === WORK) {
    const box = $('tree').querySelector('.node.selected')?.getBoundingClientRect();
    if (box) openMenu(box.left + 24, box.bottom, workMenu(v, row), $('tree'));
  } else return false;
  return true;
}

function onKey(event) {
  const keys = $('keys');
  if (!$('modal').hidden) {
    if (event.key === 'Escape') {
      event.preventDefault();
      if (!$('modal-ok').disabled) modalDone?.();
    }
    return;
  }
  if (!$('menu').hidden) {
    menuKey(event);
    return;
  }
  // Typing a commit message: the keys are the text's (Ctrl+Enter commits, by the box's own handler).
  if (event.target.tagName === 'TEXTAREA') {
    if (event.key === 'Escape') {
      event.preventDefault();
      $('tree').focus();
    }
    return;
  }
  if (event.ctrlKey && event.altKey && event.key.toLowerCase() === 'z') {
    event.preventDefault();
    const v = current ? view(current) : null;
    const row = v?.sel === WORK ? visibleRows(v).find(r => r.key === v.cursor) : null;
    if (row) rollbackFiles(rowFiles(row));
    return;
  }
  if (event.ctrlKey && !event.shiftKey && event.key.toLowerCase() === 'b') {
    event.preventDefault();
    toggleBranches();
    return;
  }
  if (event.ctrlKey && event.shiftKey && event.key.toLowerCase() === 'n') {
    event.preventDefault();
    if (current) newBranch('HEAD');
    return;
  }
  if (event.key === 'Escape') {
    event.preventDefault();
    if (!keys.hidden) keys.hidden = true;
    else if (current && view(current).diffOpen) closeDiff();
    else cancel();
    return;
  }
  if (event.key === 'F7' && !event.ctrlKey && !event.altKey) {
    event.preventDefault();
    if (current) stepChange(event.shiftKey ? -1 : 1);
    return;
  }
  if (event.key === 'F5' || (event.ctrlKey && event.key.toLowerCase() === 'r')) {
    event.preventDefault();
    if (current) read(current, true);
    return;
  }
  if (event.ctrlKey && event.key === 'Enter') {
    event.preventDefault();
    push();
    return;
  }
  if (event.ctrlKey && /^[1-9]$/.test(event.key)) {
    const repo = repos[Number(event.key) - 1];
    if (repo) {
      event.preventDefault();
      switchTo(repo.name);
    }
    return;
  }
  if (!current || event.target.tagName === 'SELECT') return;
  if (event.altKey && (event.key === 'ArrowDown' || event.key === 'ArrowUp')) {
    event.preventDefault();
    stepFile(event.key === 'ArrowDown' ? 1 : -1);
    return;
  }
  const inBranches = document.activeElement === $('branch-tree');
  if (inBranches) {
    if (branchTreeKey(event)) event.preventDefault();
    return;
  }
  const inTree = document.activeElement === $('tree');
  if (inTree && treeKey(event)) {
    event.preventDefault();
    return;
  }
  if (!inTree && (event.key === 'ArrowDown' || event.key === 'ArrowUp')) {
    event.preventDefault();
    moveCommit(event.key === 'ArrowDown' ? 1 : -1);
  }
}

function closeDiff() {
  const v = view(current);
  v.diffOpen = false;
  // The diff window closes with the diff; the next diff opens it again.
  closeDiffWindow();
  renderDiff();
  $('tree').focus();
}

/** Drag a gutter or a column edge: sizes are CSS variables, kept for the next start. */
function wireGutters() {
  const sizes = saved.get('sizes', {});
  const apply = (split, px) => {
    if (split === 'branches') $('columns').style.setProperty('--branches-size', `${px}px`);
    if (split === 'columns') $('columns').style.setProperty('--left-size', `${px}px`);
    if (split === 'message') document.querySelector('.detail').style.setProperty('--message-size', `${px}px`);
    if (split === 'diff') $('work').style.setProperty('--diff-size', `${px}px`);
    if (split === 'author' || split === 'age') $('commits').style.setProperty(`--${split}-size`, `${px}px`);
  };
  for (const [split, px] of Object.entries(sizes)) apply(split, px);
  for (const gutter of document.querySelectorAll('.gutter, .col-grip')) {
    gutter.addEventListener('pointerdown', down => {
      down.preventDefault();
      gutter.setPointerCapture(down.pointerId);
      gutter.classList.add('dragging');
      const split = gutter.dataset.split;
      // Where on the grip it was taken: the edge moves with the pointer, without a jump.
      const grab = down.clientX - gutter.parentElement.getBoundingClientRect().left;
      const move = event => {
        let px;
        if (split === 'branches') px = event.clientX - $('columns').getBoundingClientRect().left;
        if (split === 'columns') px = event.clientX - $('commits').getBoundingClientRect().left;
        if (split === 'message') px = document.querySelector('.detail').getBoundingClientRect().bottom - event.clientY;
        if (split === 'diff') px = $('work').getBoundingClientRect().bottom - event.clientY;
        // A column's grip is its left edge, and its right edge stays put: the subject takes the rest.
        if (split === 'author' || split === 'age') {
          px = gutter.parentElement.getBoundingClientRect().right - (event.clientX - grab);
          const subject = $('list-cols').querySelector('.subject').getBoundingClientRect().width;
          px = Math.min(px, gutter.parentElement.getBoundingClientRect().width + subject - 80);
        }
        px = Math.max(split === 'age' ? 40 : 60, Math.round(px));
        sizes[split] = px;
        apply(split, px);
      };
      const up = () => {
        gutter.classList.remove('dragging');
        gutter.removeEventListener('pointermove', move);
        gutter.removeEventListener('pointerup', up);
        saved.set('sizes', sizes);
      };
      gutter.addEventListener('pointermove', move);
      gutter.addEventListener('pointerup', up);
    });
  }
}

function setLayout(next) {
  layout = next;
  saved.set('layout', next);
  api.emitTo('diff', 'layout-changed', next).catch(() => {});
  renderDiff();
}

async function start() {
  api = await connect();
  await saved.load();
  layout = saved.get('layout', 'split');
  undocked = saved.get('undocked', false);
  showBranches = saved.get('branches', true);
  $('repo').onclick = () => (!$('menu').hidden && menuBack === $('repo') ? closeMenu() : repoMenu());
  $('refresh').onclick = () => current && read(current, true);
  $('update').onclick = onUpdate;
  $('push').onclick = push;
  $('cancel').onclick = cancel;
  docked = new DiffView({
    invoke: api.invoke,
    title: $('diff-title'),
    body: $('diff-body'),
    hint: $('diff-hint'),
    layout: () => layout,
    pick: followDiff,
  });
  $('close-diff').onclick = closeDiff;
  $('undock-diff').onclick = undock;
  $('view-split').onclick = () => setLayout('split');
  $('view-unified').onclick = () => setLayout('unified');
  $('prev-file').onclick = () => stepFile(-1);
  $('next-file').onclick = () => stepFile(1);
  $('expand').onclick = () => {
    if (!current) return;
    view(current).closed.clear();
    renderTree();
  };
  $('collapse').onclick = () => {
    if (!current) return;
    const v = view(current);
    for (const row of visibleRows(v)) if (row.kind === 'dir') v.closed.add(row.id);
    renderTree();
  };
  $('rollback').onclick = () => {
    if (current) rollbackFiles(tickedFiles(view(current)));
  };
  // The Uncommitted row's files: right-click for tick, diff and roll back.
  $('tree').addEventListener('contextmenu', event => {
    const node = event.target.closest('.node');
    const v = current ? view(current) : null;
    if (!node || v?.sel !== WORK) return;
    const row = visibleRows(v).find(r => r.key === node.dataset.key);
    if (!row) return;
    event.preventDefault();
    v.cursor = row.key;
    renderTree();
    $('tree').focus({ preventScroll: true });
    openMenu(event.clientX, event.clientY, workMenu(v, row), $('tree'));
  });
  $('help').onclick = event => {
    event.stopPropagation();
    $('keys').hidden = !$('keys').hidden;
  };
  document.addEventListener('click', event => {
    if (!$('keys').contains(event.target)) $('keys').hidden = true;
    if (!$('menu').contains(event.target) && !$('repo').contains(event.target)) closeMenu();
  });
  // A branch row's own handler opens the menu and prevents the default; any other right-click closes it.
  document.addEventListener('contextmenu', event => {
    if (!event.defaultPrevented && !$('menu').contains(event.target)) closeMenu();
  });
  window.addEventListener('blur', closeMenu);
  $('branch-tree').addEventListener('scroll', closeMenu);
  $('toggle-branches').onclick = toggleBranches;
  $('new-branch').onclick = () => current && newBranch('HEAD');
  $('fetch-all').onclick = () => current && fetchAll();
  $('modal-cancel').onclick = () => {
    if (!$('modal-ok').disabled) modalDone?.();
  };
  $('modal').addEventListener('pointerdown', event => {
    if (event.target === $('modal') && !$('modal-ok').disabled) modalDone?.();
  });
  document.addEventListener('keydown', onKey);
  wireGutters();
  setInterval(renderFetched, 30_000);

  await api.listen('repos-changed', event => reposChanged(event.payload || undefined));
  await api.listen('select-repo', event => loadRepos(event.payload));
  await api.listen('review', event => loadRepos(event.payload, true));
  // The diff window: it asks for its file when it loads, follows F7 into other files, docks or closes.
  await api.listen('diff-ready', () => {
    sentToWindow = null;
    const landing = windowLanding;
    windowLanding = null;
    if (undocked && windowUp) renderDiff(landing);
  });
  await api.listen('diff-picked', ({ payload }) => {
    const v = current ? view(current) : null;
    if (!undocked || !windowUp || !v || payload.name !== current || payload.sha !== v.sel) return;
    sentToWindow = { key: windowKey(diffTarget(v), payload.path) };
    v.file = payload.path;
    v.cursor = `f:${payload.path}`;
    renderTree();
    $('tree').querySelector('.node.selected')?.scrollIntoView({ block: 'nearest' });
  });
  await api.listen('diff-dock', dock);
  await api.listen('diff-close', () => current && closeDiff());
  await api.listen('diff-window-closed', () => {
    // Closed by its own close button or with the main window (docking and closing the diff
    // already cleared the flag): the diff is closed, and the next one opens the window again.
    if (!windowUp) return;
    windowUp = false;
    if (current) view(current).diffOpen = false;
    renderDiff();
  });
  // The diff window switched between side by side and unified (it saved the choice).
  await api.listen('layout-changed', ({ payload }) => {
    layout = payload;
    saved.values.layout = payload;
    if (!undocked) renderDiff();
  });
  await api.listen('repo-changed', event => {
    if (views.has(event.payload)) read(event.payload, false);
  });
  // Back in the window: the branch may have moved meanwhile (a commit from a session).
  window.addEventListener('focus', () => {
    if (!current) return;
    const v = view(current);
    if (v.inflight === 0 && !v.busy && Date.now() - v.readAt > 1500) read(current, false);
  });
  // A `dropin show` or `push-review` that started the app asked before this page could listen.
  const wanted = await api.invoke('wanted').catch(() => null);
  await loadRepos(wanted ?? undefined);
  $('list').focus();

  // The download's progress, on the dialog's button.
  await api.listen('update-progress', ({ payload }) => {
    const button = $('modal-ok');
    if (!button.disabled || !payload.total) return;
    const percent = Math.floor((payload.done / payload.total) * 100);
    button.replaceChildren(el('span', 'spinner'), `Downloading ${percent}%`);
  });
  setTimeout(() => checkUpdate(true), 5_000);
  setInterval(() => checkUpdate(true), 6 * 3600_000);
}

start();
