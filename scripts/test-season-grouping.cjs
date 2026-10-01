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

const { localAiringWeekday, formatAiring, relativeTime, seasonDateLabel } = compiled.exports;
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

for (const timezone of ['UTC', 'Asia/Shanghai', 'America/Los_Angeles']) {
  process.env.TZ = timezone;
  for (const year of [2026, 2027, 2032]) {
    for (const month of [1, 4, 7, 10]) {
      const date = new Date(Date.UTC(year, month - 1, 1));
      const anime = { source: 'bangumi', startDate: { year, month, day: 1 } };
      assert.equal(localAiringWeekday(anime, date.getTime() / 1000 + 86400), (date.getUTCDay() + 6) % 7);
      assert.ok(seasonDateLabel(anime, 'en-US').startsWith('Premiere '));
      assert.ok(seasonDateLabel(anime, 'en-US').endsWith('Time TBA'));
      assert.equal(anime.nextAiringEpisode, undefined, 'display fallback must not create an episode');
    }
  }
  assert.equal(localAiringWeekday({ broadcastWeekday: 4 }, now), 3);
  assert.equal(localAiringWeekday({ startDate: { year: 2027, month: 2, day: 30 } }, now), 7);
  assert.equal(localAiringWeekday({ startDate: { year: 2027, month: 1 } }, now), 7);
  assert.equal(localAiringWeekday({ nextAiringEpisode: { episode: 1, airingAt: dateOnly, airingPrecision: 'date' } }, dateOnly + 3600), 3);
}

console.log('Season grouping tests passed.');
