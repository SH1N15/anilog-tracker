const assert = require('node:assert/strict');
const {
  documentFromState,
  markFollowingChanged,
  markFollowingDeleted,
  markTaskChanged,
  mergeDocumentIntoState,
} = require('../electron/webdav-sync.cjs');

function state() {
  return {
    following: [{ id: 1, title: { romaji: 'One' }, displayTitle: 'One', followedAt: 10, syncUpdatedAt: 10_000 }],
    tasks: [{ id: '1-1', animeId: 1, animeTitle: 'One', episode: 1, airingAt: 20, status: 'pending', createdAt: 20, completedAt: null, syncUpdatedAt: 20_000 }],
    syncMetadata: { followingDeletedAt: {} },
  };
}

const local = state();
const remote = documentFromState(state());
remote.tasks[0].status = 'completed';
remote.tasks[0].completedAt = 30;
remote.tasks[0].syncUpdatedAt = 30_000;
remote.following.push({ id: 2, title: { romaji: 'Two' }, displayTitle: 'Two', followedAt: 25, syncUpdatedAt: 25_000 });

const merged = mergeDocumentIntoState(local, remote);
assert.equal(merged.changed, true);
assert.equal(local.following.length, 2);
assert.equal(local.tasks[0].status, 'completed');
assert.equal(merged.remoteChanged, false);

markFollowingDeleted(local, 2, 40_000);
const deleted = mergeDocumentIntoState(local, remote);
assert.equal(local.following.some((item) => item.id === 2), false);
assert.equal(deleted.remoteChanged, true);

local.following.push({ id: 2, title: { romaji: 'Two' }, displayTitle: 'Two again', followedAt: 50, syncUpdatedAt: 50_000 });
markFollowingChanged(local, 2, 50_000);
assert.equal(documentFromState(local).followingDeletedAt['2'], undefined);

// 普通本地编辑不能绕过墓碑；只有明确的重追意图才可复活。
const ordinaryState = state();
ordinaryState.following.push({ id: 2, title: { romaji: 'Two' }, displayTitle: 'Two', followedAt: 20, syncUpdatedAt: 54_000, lastChangedBy: 'local' });
markFollowingDeleted(ordinaryState, 2, 55_000);
const ordinaryEdit = mergeDocumentIntoState(ordinaryState, remote);
assert.equal(ordinaryState.following.some((item) => item.id === 2), false);
assert.equal(ordinaryEdit.remoteChanged, true);

// 明确重追意图在合并前优先于云端旧记录，并清理墓碑。
const refollowState = state();
markFollowingDeleted(refollowState, 2, 55_000);
refollowState.following.push({
  id: 2, title: { romaji: 'Two' }, displayTitle: 'Two revived', followedAt: 57,
  syncUpdatedAt: 57_000, lastChangedBy: 'local', localFollowIntentAt: 57_000,
  lastPulledFromBangumiAt: 56,
});
const refollow = mergeDocumentIntoState(refollowState, remote);
assert.equal(refollowState.following.some((item) => item.id === 2), true);
assert.equal(documentFromState(refollowState).followingDeletedAt['2'], undefined);
assert.equal(refollow.remoteChanged, true);

local.tasks.push({ id: '2-1', animeId: 2, animeTitle: 'Two again', episode: 1, airingAt: 60, status: 'pending', createdAt: 60, completedAt: null });
markTaskChanged(local, '2-1', 60_000);
assert.equal(local.tasks.find((task) => task.id === '2-1').syncUpdatedAt, 60_000);

console.log('WebDAV sync merge tests passed.');
