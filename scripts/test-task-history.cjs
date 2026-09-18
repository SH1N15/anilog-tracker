const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const ts = require('typescript');

const source = fs.readFileSync(path.join(__dirname, '..', 'src', 'task-history.ts'), 'utf8');
const compiled = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
}).outputText;
const moduleUnderTest = { exports: {} };
new Function('exports', 'module', compiled)(moduleUnderTest.exports, moduleUnderTest);
const history = moduleUnderTest.exports;
const now = Math.floor(Date.now() / 1000);
const tasks = Array.from({ length: 11 }, (_, index) => ({ episode: index + 1, status: 'completed', airingAt: now - 86400 }));
const review = { episode: 13, status: 'completed', airingAt: now + 86400,
  airingPrecision: 'instant', completionReview: { decision: 'review' } };
tasks.push(review);
assert.equal(tasks.filter(history.isCompletedHistory).length, 11);
assert.equal(tasks.filter(history.needsHistoryReview).length, 1);
assert.equal(tasks.filter(history.isPendingHistory).length, 0);
assert.equal(tasks.length, 12);
const reset = { ...review, status: 'pending', completionReview: { decision: 'reset' } };
assert(history.isScheduledHistoryReset(reset, now));
assert(!history.isPendingHistory(reset));
assert(!history.isScheduledHistoryReset(reset, now + 86400));
assert(history.isCompletedHistory({ ...review, completionReview: { decision: 'keep' } }));
console.log('Task history review counts and scheduled reset tests passed.');
