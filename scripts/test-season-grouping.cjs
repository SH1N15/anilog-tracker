const assert = require('node:assert/strict');
const Module = require('node:module');
const path = require('node:path');
const { buildSync } = require('esbuild');

process.env.TZ = 'UTC';

const root = path.join(__dirname, '..');
const output = buildSync({
  entryPoints: [path.join(root, 'src', 'utils.ts')],
  bundle: true,
  format: 'cjs',
  platform: 'node',
  define: { 'import.meta.env.VITE_ANILOG_EDITION': '"standard"' },
  write: false,
}).outputFiles[0].text;
const compiled = new Module(path.join(root, 'src', 'utils.ts'));
compiled.filename = path.join(root, 'src', 'utils.ts');
compiled.paths = Module._nodeModulePaths(root);
compiled._compile(output, compiled.filename);

const { localAiringWeekday, formatAiring, relativeTime } = compiled.exports;
const now = Date.UTC(2026, 6, 26) / 1000;
const timestamp = (year, month, day, hour = 0) => Date.UTC(year, month - 1, day, hour) / 1000;

assert.equal(localAiringWeekday({ airingSchedule: { nodes: [{ airingAt: timestamp(2026, 7, 27) }] } }, now), 0);
assert.equal(localAiringWeekday({ airingSchedule: { nodes: [{ airingAt: timestamp(2026, 8, 2) }] } }, now), 6);
assert.equal(localAiringWeekday({ airingSchedule: { nodes: [
  { airingAt: timestamp(2026, 7, 31) },
  { airingAt: timestamp(2026, 7, 28) },
] } }, now), 1);
assert.equal(localAiringWeekday({ nextAiringEpisode: { episode: 4, airingAt: timestamp(2026, 7, 29) } }, now), 2);
assert.equal(localAiringWeekday({
  airingSchedule: { nodes: [{ airingAt: timestamp(2026, 7, 31) }] },
  nextAiringEpisode: { episode: 4, airingAt: timestamp(2026, 7, 27) },
}, now), 0);
assert.equal(localAiringWeekday({ airingSchedule: { nodes: [{ airingAt: timestamp(2026, 7, 20) }] } }, now), 7);
assert.equal(localAiringWeekday({}, now), 7);
const dateOnly = timestamp(2026, 9, 10);
assert.ok(!formatAiring(dateOnly, true, 'en-US', 'date').includes(':'), 'date-only values must not display a fabricated time');
assert.equal(relativeTime(dateOnly, 'en-US', 'date'), 'Time TBA');
assert.equal(relativeTime(dateOnly, 'en-US', 'unknown'), 'Time TBA');

console.log('Season grouping tests passed.');
