// Run with `npm test` (Node's test runner; it strips the types).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { fixedParts } from '../src/lib/fixes.ts';

const marked = (text: string, fixes: Parameters<typeof fixedParts>[1]) =>
  fixedParts(text, fixes)
    .map((p) => (p.heard ? `[${p.text}<${p.heard}]` : p.text))
    .join('');

test('a fix is marked where it is, not at an earlier same word', () => {
  assert.equal(marked('Der ZB und die ZB', [{ heard: 'ZP', written: 'ZB', at: 15 }]), 'Der ZB und die [ZB<ZP]');
});

test('fixes added by a later check, before earlier ones, are all marked', () => {
  // Stored in text order by the backend, but the order doesn't matter.
  const fixes = [
    { heard: 'ZP', written: 'ZB', at: 22 },
    { heard: 'Dreh Wolin', written: 'Trevolin', at: 3 },
  ];
  assert.equal(marked('Im Trevolin liegt der ZB.', fixes), 'Im [Trevolin<Dreh Wolin] liegt der [ZB<ZP].');
});

test('positions count UTF-16 units, as the backend stores them', () => {
  assert.equal(marked('Größe: Trevolin', [{ heard: 'Drewolin', written: 'Trevolin', at: 7 }]), 'Größe: [Trevolin<Drewolin]');
});

test('a fix without a position is marked at a whole word only', () => {
  assert.equal(marked('Die ZBs und der ZB', [{ heard: 'ZP', written: 'ZB' }]), 'Die ZBs und der [ZB<ZP]');
  assert.equal(marked('Die ZBs', [{ heard: 'ZP', written: 'ZB' }]), 'Die ZBs');
});

test('a position that no longer has its word falls back to a search', () => {
  assert.equal(marked('Die ZB kam.', [{ heard: 'ZP', written: 'ZB', at: 9 }]), 'Die [ZB<ZP] kam.');
});

test('two fixes of the same word mark two places', () => {
  assert.equal(
    marked('ZB, ZB', [
      { heard: 'ZP', written: 'ZB' },
      { heard: 'ZP', written: 'ZB' },
    ]),
    '[ZB<ZP], [ZB<ZP]',
  );
});

test('no fixes: the text as one part', () => {
  assert.deepEqual(fixedParts('Hallo.', undefined), [{ text: 'Hallo.' }]);
  assert.deepEqual(fixedParts('', []), [{ text: '' }]);
});
