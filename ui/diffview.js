// One file's diff in a commit, side by side or unified, as the main window's docked panel and the
// separate diff window both draw it. F7 / Shift+F7 step through the file's changes and on into the
// commit's next or previous file, as in PhpStorm: at a file's last change F7 says so, and a second
// F7 opens the next file on its first change.

import { shapeDiff, diffRows, unifiedOf, diffStats } from './diff.js';

/** A new element with a class and text. */
function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined && text !== null) node.textContent = text;
  return node;
}

/** The `sha` of the Uncommitted row: the work tree against HEAD. */
export const WORK = '@work';

/** Commits never change, so their diffs are kept for the page's life; uncommitted ones per read. */
const cache = new Map();

/**
 * A file's diff in a commit, or in the work tree (`target.sha` WORK, read again for each new
 * `target.stamp`), shaped into rows once.
 */
export function shapedDiff(invoke, target, file) {
  const { name, sha } = target;
  const work = sha === WORK;
  const stamp = work ? target.stamp ?? 0 : 0;
  const key = `${name}\n${sha}\n${stamp}\n${file.from ?? ''}\n${file.path}`;
  if (work) {
    // An older read's diffs are stale.
    const prefix = `${name}\n${WORK}\n`;
    for (const old of cache.keys()) if (old.startsWith(prefix) && !old.startsWith(`${prefix}${stamp}\n`)) cache.delete(old);
  }
  if (!cache.has(key)) {
    const asked = work ? invoke('working_diff', { name, file }) : invoke('diff', { name, sha, file });
    cache.set(key, asked.then(({ raw, truncated }) => {
      const shaped = shapeDiff(raw);
      const rows = diffRows(shaped.hunks);
      return { ...shaped, rows, stats: diffStats(rows), truncated };
    }).catch(error => {
      cache.delete(key);
      throw error;
    }));
  }
  return cache.get(key);
}

const baseName = path => path.split('/').pop();

/** Files the diff shows as pictures, before and after, with the type the browser needs. */
const IMAGE_TYPES = {
  png: 'image/png',
  jpg: 'image/jpeg',
  jpeg: 'image/jpeg',
  gif: 'image/gif',
  webp: 'image/webp',
  bmp: 'image/bmp',
  ico: 'image/x-icon',
  avif: 'image/avif',
};
const imageType = path => IMAGE_TYPES[path.split('.').pop().toLowerCase()] ?? null;

/** "12.3 KB". */
function bytesText(n) {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

export class DiffView {
  /**
   * @param {object} o
   * @param {Function} o.invoke  the app's command call
   * @param {HTMLElement} o.title  the head's file line
   * @param {HTMLElement} o.body  the scrolling diff
   * @param {HTMLElement} o.hint  where F7 says it reached a file's edge
   * @param {Function} o.layout  'split' or 'unified'
   * @param {Function} o.pick  `(path, landing)`: F7 past a file's edge opens that file of the commit,
   *                            landing on its 'first' or 'last' change
   */
  constructor({ invoke, title, body, hint, layout, pick }) {
    Object.assign(this, { invoke, title, body, hint, layout, pick });
    this.target = null;
    this.fileKey = null;
    this.drawnKey = null;
    this.draws = 0;
    this.changes = [];
    this.at = -1;
    this.scrolledTo = 0;
    this.pending = 0;
    // Object URLs of the images drawn, freed when the body is drawn again.
    this.urls = [];
    // Scrolling by hand moves where F7 starts from, and ends a "press again" prompt.
    body.addEventListener('wheel', () => this.drop(), { passive: true });
  }

  /**
   * Draws `target` ({ name, sha, short, files, path, stamp }): `files` in the commit's tree order,
   * `path` the one to show; `sha` WORK with the read's `stamp` for the uncommitted changes. A file
   * already drawn the same way is left alone, so its scroll and F7 place stay; a new read of the
   * same uncommitted file is drawn again in place.
   */
  async show(target, landing = null) {
    this.target = target;
    const file = target.files.find(f => f.path === target.path);
    if (!file) return;
    const fileKey = `${target.name}\n${target.sha}\n${file.path}`;
    const sameFile = fileKey === this.fileKey;
    if (!sameFile) {
      this.fileKey = fileKey;
      this.at = -1;
      this.drop();
    }
    const key = `${fileKey}\n${target.stamp ?? 0}\n${file.status}\n${this.layout()}`;
    if (key === this.drawnKey && !landing) return;

    const drawing = ++this.draws;
    const where = target.sha === WORK ? 'uncommitted' : `in ${target.short ?? target.sha.slice(0, 10)}`;
    if (!sameFile) this.title.replaceChildren(el('span', 'path', file.path));
    const type = imageType(file.path);
    if (type) {
      await this.showImages(target, file, type, { key, drawing, where, sameFile, landing });
      return;
    }
    let shaped;
    try {
      shaped = await shapedDiff(this.invoke, target, file);
    } catch (error) {
      if (drawing !== this.draws) return;
      this.drawnKey = null;
      this.changes = [];
      this.free();
      this.body.replaceChildren(el('div', 'diff-note', String(error)));
      return;
    }
    // Another draw started while git ran: that one is current.
    if (drawing !== this.draws) return;

    const stats = el('span', 'stats');
    stats.append(el('span', 'plus', `+${shaped.stats.added}`), ' ', el('span', 'minus', `−${shaped.stats.removed}`));
    this.title.replaceChildren(el('span', 'path', file.path), stats, el('span', 'in', where));

    if (key !== this.drawnKey) {
      // The same file read again (an edit saved meanwhile) keeps its place.
      const keep = sameFile && this.drawnLayout === this.layout() ? this.body.scrollTop : 0;
      this.free();
      this.body.replaceChildren();
      this.body.scrollTop = 0;
      this.changes = [];
      if (shaped.truncated) this.body.append(el('div', 'diff-cut', "git's output passed 16 MB, so the end of this diff is missing."));
      if (shaped.cut) this.body.append(el('div', 'diff-cut', shaped.cut));
      if (shaped.rows.length) {
        const drawn = this.layout() === 'split' ? splitTable(shaped.rows) : unifiedTable(shaped.rows);
        this.body.append(drawn.node);
        this.changes = drawn.changes;
      } else {
        this.body.append(el('div', 'diff-note', shaped.note ?? 'No text changes to show.'));
      }
      this.drawnKey = key;
      this.drawnLayout = this.layout();
      if (keep) {
        // The changes may have moved: F7 starts again from what is in view.
        this.body.scrollTop = keep;
        this.at = -1;
      }
    }
    this.land(landing);
  }

  /** Lands on the first or last change after F7 crossed files, or stays on the change it was on. */
  land(landing) {
    if (landing) {
      const dir = landing === 'last' ? -1 : 1;
      if (this.changes.length) this.go(dir > 0 ? 0 : this.changes.length - 1);
      else this.edge(dir);
    } else if (this.at >= 0 && this.at < this.changes.length) {
      // The same file in the other layout: stay on the change it was on.
      this.go(this.at);
    }
  }

  /**
   * An image's before and after, side by side on a checkerboard, with each one's size. A new file
   * has no before, a deleted one no after. It has no changes for F7, which steps on to the next file.
   */
  async showImages(target, file, type, { key, drawing, where, sameFile, landing }) {
    if (key === this.drawnKey) {
      this.land(landing);
      return;
    }
    const work = target.sha === WORK;
    const read = (rev, path) => this.invoke('image', { name: target.name, rev, path }).then(bytes => new Blob([bytes], { type }));
    const sides = [
      { label: 'Before', has: file.status !== 'A' && file.status !== '?', rev: work ? 'HEAD' : `${target.sha}^`, path: file.from ?? file.path },
      { label: 'After', has: file.status !== 'D', rev: work ? '' : target.sha, path: file.path },
    ];
    const blobs = await Promise.allSettled(sides.map(side => (side.has ? read(side.rev, side.path) : Promise.resolve(null))));
    if (drawing !== this.draws) return;

    this.title.replaceChildren(el('span', 'path', file.path), el('span', 'in', where));
    this.free();
    this.changes = [];
    const pair = el('div', 'image-diff');
    sides.forEach((side, i) => {
      const figure = el('figure', `image-side ${i ? 'new' : 'old'}`);
      const caption = el('figcaption');
      caption.append(el('span', 'label', side.label));
      const frame = el('div', 'image-frame');
      const got = blobs[i];
      if (!side.has) {
        frame.append(el('div', 'diff-note', i ? 'Deleted' : 'New file'));
        frame.classList.add('empty');
      } else if (got.status === 'rejected') {
        frame.append(el('div', 'diff-note', String(got.reason)));
        frame.classList.add('empty');
      } else {
        const url = URL.createObjectURL(got.value);
        this.urls.push(url);
        const img = el('img');
        img.alt = `${side.label}: ${baseName(side.path)}`;
        const size = el('span', 'size', bytesText(got.value.size));
        img.addEventListener('load', () => size.textContent = `${img.naturalWidth} × ${img.naturalHeight} · ${bytesText(got.value.size)}`);
        img.addEventListener('error', () => frame.replaceChildren(el('div', 'diff-note', 'This image cannot be shown.')));
        img.src = url;
        frame.append(img);
        caption.append(size);
      }
      figure.append(caption, frame);
      pair.append(figure);
    });
    this.body.replaceChildren(pair);
    if (!sameFile) this.body.scrollTop = 0;
    this.drawnKey = key;
    this.drawnLayout = this.layout();
    this.land(landing);
  }

  /** Frees the images the last draw made. */
  free() {
    for (const url of this.urls) URL.revokeObjectURL(url);
    this.urls = [];
  }

  /** Forgets what is drawn, so the next show draws again; any draw still running is dropped. */
  reset() {
    this.draws++;
    this.drawnKey = null;
    this.fileKey = null;
    this.drop();
  }

  /** F7 (dir 1) or Shift+F7 (dir -1). */
  step(dir) {
    if (!this.target || !this.drawnKey) return;
    if (this.pending === dir) {
      const next = this.neighbour(dir);
      this.drop();
      if (next) this.pick(next.path, dir > 0 ? 'first' : 'last');
      return;
    }
    const next = this.nextChange(dir);
    if (next === null) this.edge(dir);
    else this.go(next);
  }

  /** The change F7 goes to, or null past the file's last one. */
  nextChange(dir) {
    const count = this.changes.length;
    if (!count) return null;
    const tracked = this.at >= 0 && Math.abs(this.body.scrollTop - this.scrolledTo) < 2;
    if (tracked) {
      const next = this.at + dir;
      return next >= 0 && next < count ? next : null;
    }
    // Scrolled by hand, or nothing picked yet: start from what is in view.
    const top = this.body.scrollTop;
    const tops = this.changes.map(rows => this.topOf(rows[0]));
    if (dir > 0) {
      const next = tops.findIndex(t => t >= top);
      return next < 0 ? null : next;
    }
    const before = tops.findLastIndex(t => t < top);
    return before < 0 ? null : before;
  }

  /** Moves to change `i`: scrolled into the upper part of the view and marked. */
  go(i) {
    this.drop();
    this.at = i;
    for (const tr of this.body.querySelectorAll('tr.current-change')) tr.classList.remove('current-change');
    const rows = this.changes[i];
    for (const tr of rows) tr.classList.add('current-change');
    const top = this.topOf(rows[0]);
    const height = this.topOf(rows[rows.length - 1]) + rows[rows.length - 1].offsetHeight - top;
    const room = this.body.clientHeight;
    this.body.scrollTop = Math.max(0, height < room * 0.7 ? top - room * 0.25 : top - 24);
    this.scrolledTo = this.body.scrollTop;
  }

  /** No change left that way: say so, and arm the next press to cross into the neighbouring file. */
  edge(dir) {
    const next = this.neighbour(dir);
    const key = dir > 0 ? 'F7' : 'Shift+F7';
    const none = this.changes.length ? (dir > 0 ? 'No more changes in this file.' : 'No earlier changes in this file.') : 'No changes to step through in this file.';
    if (next) {
      this.say(`${none} Press ${key} again for ${baseName(next.path)}.`);
      this.pending = dir;
    } else {
      const scope = this.target.sha === WORK ? 'the uncommitted changes' : 'this commit';
      this.say(dir > 0 ? `No more changes in ${scope}.` : `No earlier changes in ${scope}.`);
      this.pending = 0;
    }
  }

  /** The commit's file after (dir 1) or before (dir -1) the shown one. */
  neighbour(dir) {
    const files = this.target?.files ?? [];
    const at = files.findIndex(f => f.path === this.target.path);
    return at < 0 ? null : (files[at + dir] ?? null);
  }

  say(text) {
    this.hint.textContent = text;
    this.hint.hidden = false;
  }

  /** Ends a "press again" prompt. */
  drop() {
    this.pending = 0;
    this.hint.hidden = true;
  }

  /** A row's top inside the scrolling body. */
  topOf(tr) {
    return tr.getBoundingClientRect().top - this.body.getBoundingClientRect().top + this.body.scrollTop;
  }
}

/** A cell's text, with changed words in <mark>. */
function textCell(cell, side) {
  const td = el('td', `text ${side}`);
  if (!cell) {
    td.classList.add('gap');
    return td;
  }
  if (cell.kind !== 'context') td.classList.add(cell.kind);
  for (const seg of cell.segs) td.append(seg.isChanged ? el('mark', '', seg.text) : document.createTextNode(seg.text));
  return td;
}

function noCell(n, cell) {
  const td = el('td', 'no', n ?? '');
  if (!cell) td.classList.add('gap');
  else if (cell.kind !== 'context') td.classList.add(cell.kind);
  return td;
}

function hunkRow(row, span) {
  const tr = el('tr', 'hunk');
  const td = el('td', '', `@@ −${row.oldStart} +${row.newStart} @@${row.context ? `  ${row.context}` : ''}`);
  td.colSpan = span;
  tr.append(td);
  return tr;
}

/**
 * Numbers the changes: a run of rows that add or remove lines, broken by unchanged lines and hunk
 * heads, is one change. `ids[i]` is row i's change, or -1.
 */
function changeIds(lines, changed) {
  const ids = [];
  let count = 0;
  let inside = false;
  for (const line of lines) {
    const is = line.kind !== 'hunk' && changed(line);
    if (is && !inside) count++;
    inside = is;
    ids.push(is ? count - 1 : -1);
  }
  return { ids, count };
}

/**
 * Side by side: one table per side, each scrolling sideways on its own (as in PhpStorm), so a
 * long line on one side does not push the other out of view. Every row is one line high, so the
 * two stay level while the diff scrolls down.
 */
function splitTable(rows) {
  const { ids, count } = changeIds(rows, row => [row.left, row.right].some(c => c && c.kind !== 'context'));
  const changes = Array.from({ length: count }, () => []);
  const panes = el('div', 'split-panes');
  for (const side of ['left', 'right']) {
    const table = el('table', `code split ${side === 'left' ? 'old' : 'new'}`);
    const body = el('tbody');
    rows.forEach((row, i) => {
      if (row.kind === 'hunk') {
        body.append(hunkRow(row, 2));
        return;
      }
      const tr = el('tr');
      const cell = row[side];
      tr.append(noCell(cell?.no, cell), textCell(cell, side === 'left' ? 'old' : 'new'));
      if (ids[i] >= 0) changes[ids[i]].push(tr);
      body.append(tr);
    });
    table.append(body);
    const pane = el('div', 'pane');
    pane.append(table);
    panes.append(pane);
  }
  // Each change's rows in screen order: the left side's first, which is where its top is measured.
  return { node: panes, changes };
}

function unifiedTable(rows) {
  const lines = unifiedOf(rows);
  const { ids, count } = changeIds(lines, line => line.cell.kind !== 'context');
  const changes = Array.from({ length: count }, () => []);
  const table = el('table', 'code unified');
  const body = el('tbody');
  lines.forEach((line, i) => {
    if (line.kind === 'hunk') {
      body.append(hunkRow(line, 4));
      return;
    }
    const tr = el('tr');
    const kind = line.cell.kind;
    const mark = el('td', 'mark', kind === 'added' ? '+' : kind === 'removed' ? '−' : '');
    if (kind !== 'context') mark.classList.add(kind);
    tr.append(noCell(line.oldNo, line.cell), noCell(line.newNo, line.cell), mark, textCell(line.cell, 'one'));
    if (ids[i] >= 0) changes[ids[i]].push(tr);
    body.append(tr);
  });
  table.append(body);
  return { node: table, changes };
}
