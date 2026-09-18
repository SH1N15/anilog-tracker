const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const ts = require('typescript');

const source = fs.readFileSync(path.join(__dirname, '..', 'src', 'state-refresh.ts'), 'utf8');
const compiled = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
}).outputText;
const moduleUnderTest = { exports: {} };
new Function('exports', 'module', compiled)(moduleUnderTest.exports, moduleUnderTest);
const { createStateRefreshController } = moduleUnderTest.exports;

async function main() {
  let pushed;
  let resolveInitial;
  let unsubscribed = false;
  const applied = [];
  const order = [];
  const controller = createStateRefreshController({
    getState: () => {
      order.push('read');
      return new Promise((resolve) => { resolveInitial = resolve; });
    },
    subscribe: (callback) => {
      order.push('subscribe');
      pushed = callback;
      return () => { unsubscribed = true; };
    },
    applyState: (state) => applied.push(state),
  });

  const initialRead = controller.refresh();
  assert.deepEqual(order, ['subscribe', 'read']);
  pushed({ value: 'new push' });
  resolveInitial({ value: 'stale read' });
  await initialRead;
  assert.deepEqual(applied, [{ value: 'new push' }]);

  controller.dispose();
  assert.equal(unsubscribed, true);

  let reads = 0;
  const recovered = [];
  const errors = [];
  const retrying = createStateRefreshController({
    getState: async () => {
      reads++;
      if (reads < 3) throw new Error('Native state is not ready');
      return { following: [{ id: 607340 }], bangumiSyncSettings: { syncEnabled: true } };
    },
    subscribe: () => () => {},
    applyState: (state) => recovered.push(state),
    onError: (error) => errors.push(error),
    retryDelaysMs: [1, 1],
  });
  await retrying.refresh(true);
  assert.equal(reads, 3, 'cold startup retries without a focus/visibility event');
  assert.equal(recovered.length, 1, 'only a real state snapshot is applied');
  assert.equal(recovered[0].bangumiSyncSettings.syncEnabled, true);
  assert.equal(recovered[0].following[0].id, 607340);
  assert.deepEqual(errors, [], 'transient startup errors are not shown as data loss');
  retrying.dispose();

  let exhaustedReads = 0;
  const exhaustedErrors = [];
  const exhausted = createStateRefreshController({
    getState: async () => { exhaustedReads++; throw new Error('Read failed'); },
    subscribe: () => () => {},
    applyState: () => assert.fail('failure must not apply an empty fallback'),
    onError: (error) => exhaustedErrors.push(error.message),
    retryDelaysMs: [1, 1],
  });
  await exhausted.refresh(true);
  assert.equal(exhaustedReads, 3);
  assert.deepEqual(exhaustedErrors, ['Read failed']);
  exhausted.dispose();

  for (const stop of ['push', 'dispose']) {
    let callback;
    let failedReads = 0;
    const snapshots = [];
    const interrupted = createStateRefreshController({
      getState: async () => { failedReads++; throw new Error('Not ready'); },
      subscribe: (listener) => { callback = listener; return () => {}; },
      applyState: (state) => snapshots.push(state),
      onError: () => assert.fail('cancelled startup must not report an error'),
      retryDelaysMs: [100],
    });
    const pending = interrupted.refresh(true);
    await new Promise((resolve) => setImmediate(resolve));
    if (stop === 'push') callback({ restored: true });
    else interrupted.dispose();
    await pending;
    assert.equal(failedReads, 1, 'a push or disposal cancels pending retries');
    assert.deepEqual(snapshots, stop === 'push' ? [{ restored: true }] : []);
    interrupted.dispose();
  }

  let readyReads = 0;
  const retained = [];
  const ready = createStateRefreshController({
    getState: async () => {
      if (++readyReads > 1) throw new Error('Temporary resume failure');
      return { following: [42], syncEnabled: true };
    },
    subscribe: () => () => {},
    applyState: (state) => retained.push(state),
    retryDelaysMs: [1, 1],
  });
  await ready.refresh();
  await ready.refresh();
  assert.equal(readyReads, 2, 'normal refreshes do not introduce background polling');
  assert.deepEqual(retained, [{ following: [42], syncEnabled: true }]);
  ready.dispose();
  console.log('State refresh startup, retry, retention and race tests passed.');
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
