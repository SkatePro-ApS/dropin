// Light syntax colouring for the diff: comments, strings, numbers, keywords, types, calls and
// variables, picked by the file's extension.
// shortcut: each line is read on its own, so a block comment or string spanning lines is caught only
// on its first line (and on lines starting with `*`, as docblocks do); a real lexer per language if that bites.

const C_LIKE = { line: ['//'], block: ['/*', '*/'], quotes: '"\'`' };
const HASH = { line: ['#'], quotes: '"\'' };
const LANGS = {
  js: C_LIKE, mjs: C_LIKE, cjs: C_LIKE, jsx: C_LIKE, ts: C_LIKE, tsx: C_LIKE, java: C_LIKE, kt: C_LIKE,
  c: C_LIKE, h: C_LIKE, cpp: C_LIKE, hpp: C_LIKE, cs: C_LIKE, go: C_LIKE, swift: C_LIKE, dart: C_LIKE,
  scss: C_LIKE, less: C_LIKE, json: C_LIKE,
  rs: { line: ['//'], block: ['/*', '*/'], quotes: '"' },
  php: { line: ['//', '#'], block: ['/*', '*/'], quotes: '"\'`', vars: true },
  css: { block: ['/*', '*/'], quotes: '"\'' },
  py: HASH, rb: HASH, yml: HASH, yaml: HASH, toml: HASH, ini: { line: [';', '#'], quotes: '"\'' },
  sh: { ...HASH, vars: true }, bash: { ...HASH, vars: true }, ps1: { ...HASH, vars: true },
  sql: { line: ['--'], block: ['/*', '*/'], quotes: '"\'`', anyCase: true },
  lua: { line: ['--'], quotes: '"\'' },
  html: { block: ['<!--', '-->'], quotes: '"\'', tags: true },
  xml: { block: ['<!--', '-->'], quotes: '"\'', tags: true },
  svg: { block: ['<!--', '-->'], quotes: '"\'', tags: true },
  vue: { line: ['//'], block: ['<!--', '-->'], quotes: '"\'`', tags: true },
  twig: { block: ['{#', '#}'], quotes: '"\'', tags: true },
};

const KEYWORDS = new Set(`abstract and as async await break case catch class const continue def default
  defer del delete do echo elif else elseif enum except export extends false final finally fn for foreach
  from func function global go goto if impl implements import in include instanceof interface is lambda let
  loop match mod module mut namespace new nil none not null or package pass private protected pub public
  raise readonly require require_once return self static struct super switch this throw throws trait true
  try type typeof use var void where while with yield True False None
  select update insert into values set delete join left right inner outer on group by order having limit
  union distinct case when then end create table alter drop index primary key references`.split(/\s+/));

const esc = text => text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
const rules = new Map();

/** One regex per language: comment | string | number | tag | word, in that order of groups. */
function ruleOf(lang) {
  if (rules.has(lang)) return rules.get(lang);
  const spec = LANGS[lang];
  const comments = [
    ...(spec.block ? [`${esc(spec.block[0])}[\\s\\S]*?(?:${esc(spec.block[1])}|$)`] : []),
    ...(spec.line ?? []).map(l => `${esc(l)}.*`),
  ];
  const strings = [...spec.quotes].map(q => `${q}(?:\\\\.|[^${q}\\\\])*${q}`);
  const rule = new RegExp([
    `(${comments.join('|') || '(?!)'})`,
    `(${strings.join('|')})`,
    '(\\b(?:0[xX][\\da-fA-F_]+|\\d[\\d_]*(?:\\.\\d+)?(?:[eE][+-]?\\d+)?)\\b)',
    spec.tags ? '(</?[\\w:-]+)' : '((?!))',
    `(${spec.vars ? '\\$?' : ''}[A-Za-z_][\\w]*)`,
  ].join('|'), 'g');
  rules.set(lang, rule);
  return rule;
}

/** The language key for a path, or null for files left plain. */
export function langOf(path) {
  const ext = path.split('.').pop().toLowerCase();
  return LANGS[ext] ? ext : null;
}

/** A line's coloured runs: `[start, end, class]`, in order and not overlapping. */
export function paint(line, lang) {
  if (!lang) return [];
  const spec = LANGS[lang];
  if (spec.block?.[0] === '/*' && /^\s*\*/.test(line)) return [[0, line.length, 'comment']];
  const runs = [];
  for (const m of line.matchAll(ruleOf(lang))) {
    const [text, comment, string, number, tag, word] = m;
    const end = m.index + text.length;
    let kind = comment ? 'comment' : string ? 'string' : number ? 'number' : tag ? 'tag' : null;
    if (word) {
      const bare = spec.anyCase ? word.toLowerCase() : word;
      if (word[0] === '$') kind = 'var';
      else if (KEYWORDS.has(bare)) kind = 'keyword';
      else if (/^\s*\(/.test(line.slice(end))) kind = 'call';
      else if (/^[A-Z][a-z]/.test(word)) kind = 'type';
    }
    if (kind) runs.push([m.index, end, kind]);
  }
  return runs;
}
