// The diff in its own window. The main window tells it which file of which commit to show
// (`diff-show`) and keeps its tree on the file F7 moves to here (`diff-picked`).

import { DiffView } from './diffview.js';

const $ = id => document.getElementById(id);
const tauri = window.__TAURI__;
const { invoke } = tauri.core;
const { listen, emitTo } = tauri.event;
const win = tauri.window.getCurrentWindow();

/** Side by side or unified: one setting for both windows, kept by the app (ui.json). */
let shape = (await invoke('prefs').catch(() => ({}))).layout ?? 'split';
const layout = () => shape;

function setLayout(next) {
  shape = next;
  invoke('set_pref', { key: 'layout', value: next }).catch(() => {});
  emitTo('main', 'layout-changed', next).catch(() => {});
  draw();
}

let target = null;

const diff = new DiffView({
  invoke,
  title: $('diff-title'),
  body: $('diff-body'),
  hint: $('diff-hint'),
  layout,
  pick: (path, landing) => open(path, landing),
});

/** Shows another file of the same commit and tells the main window. */
function open(path, landing = null) {
  if (!target) return;
  target = { ...target, path };
  draw(landing);
  emitTo('main', 'diff-picked', { name: target.name, sha: target.sha, path }).catch(() => {});
}

function draw(landing = null) {
  $('view-split').classList.toggle('on', layout() === 'split');
  $('view-unified').classList.toggle('on', layout() === 'unified');
  if (!target) {
    diff.reset();
    $('diff-title').replaceChildren();
    const note = document.createElement('div');
    note.className = 'diff-note';
    note.textContent = 'Pick a file in the main window to see its diff here.';
    $('diff-body').replaceChildren(note);
    win.setTitle('Diff').catch(() => {});
    return;
  }
  win.setTitle(`${target.path.split('/').pop()} · ${target.name} · Diff`).catch(() => {});
  diff.show(target, landing);
}

/** Alt+↑/↓: the commit's previous or next file, round the end. */
function stepFile(step) {
  if (!target?.files.length) return;
  const files = target.files;
  const at = files.findIndex(f => f.path === target.path);
  open(files[at < 0 ? 0 : (at + step + files.length) % files.length].path);
}

document.addEventListener('keydown', event => {
  if (event.key === 'F7' && !event.ctrlKey && !event.altKey) {
    event.preventDefault();
    diff.step(event.shiftKey ? -1 : 1);
  } else if (event.altKey && (event.key === 'ArrowDown' || event.key === 'ArrowUp')) {
    event.preventDefault();
    stepFile(event.key === 'ArrowDown' ? 1 : -1);
  } else if (event.key === 'Escape') {
    event.preventDefault();
    emitTo('main', 'diff-close').catch(() => {});
  }
});

$('view-split').onclick = () => setLayout('split');
$('view-unified').onclick = () => setLayout('unified');
$('prev-file').onclick = () => stepFile(-1);
$('next-file').onclick = () => stepFile(1);
$('dock-diff').onclick = () => emitTo('main', 'diff-dock').catch(() => {});
// The main window switched layout.
await listen('layout-changed', ({ payload }) => {
  shape = payload;
  draw();
});

await listen('diff-show', ({ payload }) => {
  target = payload.target;
  draw(payload.landing);
});
await listen('diff-step', ({ payload }) => diff.step(payload));
await emitTo('main', 'diff-ready');
draw();
$('diff-body').focus();
