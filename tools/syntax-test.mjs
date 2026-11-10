// Checks the diff's syntax colouring: node tools/syntax-test.mjs
import assert from 'node:assert';
import { paint, langOf } from '../ui/syntax.js';

const show = (line, lang) => paint(line, lang).map(([s, e, k]) => `${k}:${line.slice(s, e)}`);
assert.deepStrictEqual(show('return "a" // b', 'js'), ['keyword:return', 'string:"a"', 'comment:// b']);
assert.deepStrictEqual(show('$x = foo(\'a#b\', 42); # c', 'php'), ['var:$x', 'call:foo', "string:'a#b'", 'number:42', 'comment:# c']);
assert.deepStrictEqual(show(' * @param string $x', 'php'), ['comment: * @param string $x']);
assert.deepStrictEqual(show('SELECT id FROM t', 'sql'), ['keyword:SELECT', 'keyword:FROM']);
assert.deepStrictEqual(paint('anything', null), []);
assert.equal(langOf('a/b.PHP'), 'php');
assert.equal(langOf('README.md'), null);
console.log('syntax ok');
