// Diff shaping: a raw `git show <sha> -- <path>`
// becomes hunks, then rows for the side-by-side and one-column views, with changed words marked.

/** Characters of hunks drawn at most; a window can hold far more than the mod's panel. */
export const DIFF_BUDGET = 400_000;

/** A minified file is one huge line: cut each line here so a hunk still fits. */
export const MAX_LINE = 2_000;

/** Drops control characters; tab and newline stay (CRLF files lose the CR). */
export const clean = text => text.replace(/[\u0000-\u0008\u000b-\u001f\u007f-\u009f]/g, '');

const thousands = n => n.toLocaleString('en-US');
const plural = (n, word) => `${thousands(n)} ${word}${n === 1 ? '' : 's'}`;

/**
 * Drops the file headers (the hunks start at the first `@@`), cuts long lines, and keeps whole
 * hunks up to `budget` characters. A first hunk larger than the budget is cut short with its
 * `@@` counts redone. Returns `{ hunks, note?, cut? }`.
 */
export function shapeDiff(raw, budget = DIFF_BUDGET) {
  const lines = clean(raw).split('\n');
  const first = lines.findIndex(line => line.startsWith('@@ '));
  if (first < 0) return { hunks: '', note: noteFor(lines) };

  const hunks = [];
  let inHeader = false;
  for (const line of lines.slice(first)) {
    if (line.startsWith('diff --git ')) {
      inHeader = true;
      continue;
    }
    if (line.startsWith('@@ ')) {
      inHeader = false;
      hunks.push([line]);
      continue;
    }
    const hunk = hunks[hunks.length - 1];
    if (inHeader || line === '' || hunk === undefined || !/^[ +\-\\]/.test(line)) continue;
    hunk.push(line.length > MAX_LINE ? `${line.slice(0, MAX_LINE)} … (line cut)` : line);
  }

  const kept = [];
  let size = 0;
  for (const hunk of hunks) {
    const length = hunk.reduce((n, line) => n + line.length + 1, 0);
    if (size + length > budget) {
      if (kept.length === 0) kept.push(cutHunk(hunk, budget));
      break;
    }
    kept.push(hunk);
    size += length;
  }

  const linesOf = list => list.reduce((n, hunk) => n + hunk.length - 1, 0);
  const shown = linesOf(kept);
  const total = linesOf(hunks);
  const shaped = { hunks: kept.map(hunk => hunk.join('\n')).join('\n') };
  if (shown < total) {
    shaped.cut = `Showing ${thousands(shown)} of ${thousands(total)} lines (${kept.length} of ${plural(hunks.length, 'hunk')}).`;
  }
  return shaped;
}

function cutHunk(hunk, budget) {
  const header = hunk[0] ?? '@@ -0,0 +0,0 @@';
  const body = [];
  let size = header.length + 1;
  for (const line of hunk.slice(1)) {
    if (size + line.length + 1 > budget) break;
    body.push(line);
    size += line.length + 1;
  }
  return [recount(header, body), ...body];
}

/** `@@ -a,b +c,d @@` with b and d counted again from the lines kept. */
export function recount(header, body) {
  const match = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@(.*)$/.exec(header);
  if (!match) return header;
  const before = body.filter(line => line.startsWith(' ') || line.startsWith('-')).length;
  const after = body.filter(line => line.startsWith(' ') || line.startsWith('+')).length;
  return `@@ -${match[1]},${before} +${match[2]},${after} @@${match[3]}`;
}

/** Tabs drawn as four columns. */
const untab = text => text.replace(/\t/g, '    ');

/**
 * Rows of the side-by-side view: `{kind:'hunk', oldStart, newStart, context}` or
 * `{kind:'line', left?, right?}`, a cell being `{no, kind:'context'|'removed'|'added', segs}`.
 * A changed line sits beside the line that replaced it; a pure removal or addition leaves
 * the other side empty.
 */
export function diffRows(hunks) {
  const rows = [];
  let oldNo = 0;
  let newNo = 0;
  let removed = [];
  let added = [];
  const flush = () => {
    for (let i = 0; i < Math.max(removed.length, added.length); i++) {
      let left = removed[i];
      let right = added[i];
      if (left !== undefined && right !== undefined) {
        const [a, b] = wordDiff(textOf(left), textOf(right));
        left = { ...left, segs: a };
        right = { ...right, segs: b };
      }
      rows.push({ kind: 'line', left, right });
    }
    removed = [];
    added = [];
  };

  for (const line of hunks.split('\n')) {
    const head = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@ ?(.*)$/.exec(line);
    if (head) {
      flush();
      oldNo = Number(head[1]);
      newNo = Number(head[2]);
      rows.push({ kind: 'hunk', oldStart: oldNo, newStart: newNo, context: untab(head[3] ?? '').trim() });
      continue;
    }
    const text = untab(line.slice(1));
    const mark = line.charAt(0);
    if (mark === ' ') {
      flush();
      rows.push({ kind: 'line', left: { no: oldNo++, kind: 'context', segs: [{ text }] }, right: { no: newNo++, kind: 'context', segs: [{ text }] } });
    } else if (mark === '-') {
      if (added.length > 0) flush();
      removed.push({ no: oldNo++, kind: 'removed', segs: [{ text }] });
    } else if (mark === '+') {
      added.push({ no: newNo++, kind: 'added', segs: [{ text }] });
    }
    // `\ No newline at end of file` draws nothing.
  }
  flush();
  return rows;
}

export const textOf = cell => cell.segs.map(seg => seg.text).join('');

/**
 * The side-by-side rows in one column: each change's removed lines, then its added ones.
 * A line is `{kind:'hunk', ...}` or `{kind:'line', oldNo?, newNo?, cell}`.
 */
export function unifiedOf(rows) {
  const lines = [];
  let removed = [];
  let added = [];
  const flush = () => {
    for (const cell of removed) lines.push({ kind: 'line', oldNo: cell.no, cell });
    for (const cell of added) lines.push({ kind: 'line', newNo: cell.no, cell });
    removed = [];
    added = [];
  };
  for (const row of rows) {
    if (row.kind === 'hunk') {
      flush();
      lines.push(row);
    } else if (row.left?.kind === 'context' && row.right !== undefined) {
      flush();
      lines.push({ kind: 'line', oldNo: row.left.no, newNo: row.right.no, cell: row.right });
    } else {
      if (row.left !== undefined) removed.push(row.left);
      if (row.right !== undefined) added.push(row.right);
    }
  }
  flush();
  return lines;
}

/** Lines added and removed in the rows. */
export function diffStats(rows) {
  let added = 0;
  let removed = 0;
  for (const row of rows) {
    if (row.kind !== 'line') continue;
    if (row.left?.kind === 'removed') removed++;
    if (row.right?.kind === 'added') added++;
  }
  return { added, removed };
}

/** Words, runs of spaces, and single punctuation marks: what a changed-word mark covers. */
const TOKEN = /\s+|[\p{L}\p{N}_$]+|[^\s\p{L}\p{N}_$]/gu;

/** Comparing two lines costs their token counts multiplied; past this, a line is marked whole. */
const WORD_DIFF_CELLS = 250_000;

/**
 * The words that differ between a removed line and the line that replaced it, by the longest
 * common run of tokens. A pair with little in common is marked as wholly changed instead:
 * word marks scattered over a rewritten line only add noise.
 */
export function wordDiff(before, after) {
  const whole = [[{ text: before }], [{ text: after }]];
  const x = before.match(TOKEN) ?? [];
  const y = after.match(TOKEN) ?? [];
  const n = x.length;
  const m = y.length;
  if (n === 0 || m === 0 || (n + 1) * (m + 1) > WORD_DIFF_CELLS) return whole;

  // lcs[i][j]: the common run of x[i..] and y[j..], kept flat.
  const width = m + 1;
  const lcs = new Uint16Array((n + 1) * width);
  const at = (i, j) => lcs[i * width + j] ?? 0;
  for (let i = n - 1; i >= 0; i--) {
    for (let j = m - 1; j >= 0; j--) {
      lcs[i * width + j] = x[i] === y[j] ? at(i + 1, j + 1) + 1 : Math.max(at(i + 1, j), at(i, j + 1));
    }
  }

  const left = [];
  const right = [];
  const add = (list, text, isChanged) => {
    const last = list[list.length - 1];
    if (last !== undefined && (last.isChanged === true) === isChanged) last.text += text;
    else list.push(isChanged ? { text, isChanged: true } : { text });
  };
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (x[i] === y[j]) {
      add(left, x[i], false);
      add(right, y[j], false);
      i++;
      j++;
    } else if (at(i + 1, j) >= at(i, j + 1)) {
      add(left, x[i], true);
      i++;
    } else {
      add(right, y[j], true);
      j++;
    }
  }
  for (; i < n; i++) add(left, x[i], true);
  for (; j < m; j++) add(right, y[j], true);

  const kept = left.filter(seg => seg.isChanged !== true).reduce((k, seg) => k + seg.text.trim().length, 0);
  if (kept < Math.min(before.trim().length, after.trim().length) * 0.3) return whole;
  return [left, right];
}

/** What to say for a change that has no hunks. */
function noteFor(lines) {
  const value = prefix => lines.find(line => line.startsWith(prefix))?.slice(prefix.length);
  if (lines.some(line => line.startsWith('Binary files ') || line === 'GIT binary patch')) return 'Binary file: there is no text diff to show.';
  const from = value('rename from ');
  if (from !== undefined) return `Renamed from ${from}; the content is unchanged.`;
  const oldMode = value('old mode ');
  const newMode = value('new mode ');
  if (oldMode !== undefined && newMode !== undefined) return `File mode changed from ${oldMode} to ${newMode}.`;
  if (lines.some(line => line.startsWith('new file mode'))) return 'New empty file.';
  if (lines.some(line => line.startsWith('deleted file mode'))) return 'Deleted an empty file.';
  return 'No text changes to show.';
}
