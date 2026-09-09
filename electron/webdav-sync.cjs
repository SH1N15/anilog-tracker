const SYNC_DOCUMENT_VERSION = 1;

function asTimestamp(value, fallback = 0) {
  const number = Number(value);
  return Number.isFinite(number) && number > 0 ? Math.floor(number) : fallback;
}

function recordTimestamp(record, fallbackField) {
  const explicit = asTimestamp(record?.syncUpdatedAt);
  if (explicit) return explicit;
  const fallback = asTimestamp(record?.[fallbackField]);
  return fallback ? fallback * 1000 : 0;
}

function clone(value) {
  return value == null ? value : structuredClone(value);
}

function stableRecord(record) {
  if (!record || typeof record !== 'object') return '';
  return JSON.stringify(Object.fromEntries(Object.entries(record).sort(([left], [right]) => left.localeCompare(right))));
}

function chooseRecord(left, right, fallbackField) {
  if (!left) return right ? clone(right) : null;
  if (!right) return clone(left);
  const leftTimestamp = recordTimestamp(left, fallbackField);
  const rightTimestamp = recordTimestamp(right, fallbackField);
  if (leftTimestamp !== rightTimestamp) return clone(leftTimestamp > rightTimestamp ? left : right);
  return clone(stableRecord(left) >= stableRecord(right) ? left : right);
}

function normalizeTombstones(value) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return {};
  return Object.fromEntries(
    Object.entries(value)
      .map(([id, timestamp]) => [String(Number(id)), asTimestamp(timestamp)])
      .filter(([id, timestamp]) => Number(id) > 0 && timestamp > 0),
  );
}

function ensureSyncMetadata(state) {
  if (!state.syncMetadata || typeof state.syncMetadata !== 'object') state.syncMetadata = {};
  state.syncMetadata.followingDeletedAt = normalizeTombstones(state.syncMetadata.followingDeletedAt);
  state.following = Array.isArray(state.following) ? state.following : [];
  state.tasks = Array.isArray(state.tasks) ? state.tasks : [];
  state.following.forEach((item) => {
    if (!asTimestamp(item.syncUpdatedAt)) item.syncUpdatedAt = recordTimestamp(item, 'followedAt') || Date.now();
  });
  state.tasks.forEach((task) => {
    if (!asTimestamp(task.syncUpdatedAt)) {
      task.syncUpdatedAt = Math.max(recordTimestamp(task, 'createdAt'), asTimestamp(task.completedAt) * 1000) || Date.now();
    }
  });
  return state.syncMetadata;
}

function markFollowingChanged(state, animeId, timestamp = Date.now()) {
  const metadata = ensureSyncMetadata(state);
  const item = state.following.find((entry) => entry.id === animeId);
  if (item) item.syncUpdatedAt = timestamp;
  if (asTimestamp(metadata.followingDeletedAt[String(animeId)]) <= timestamp) {
    delete metadata.followingDeletedAt[String(animeId)];
  }
}

function markFollowingDeleted(state, animeId, timestamp = Date.now()) {
  const metadata = ensureSyncMetadata(state);
  metadata.followingDeletedAt[String(animeId)] = Math.max(
    asTimestamp(metadata.followingDeletedAt[String(animeId)]),
    timestamp,
  );
}

function markTaskChanged(state, taskId, timestamp = Date.now()) {
  ensureSyncMetadata(state);
  const task = state.tasks.find((entry) => entry.id === taskId);
  if (task) task.syncUpdatedAt = timestamp;
}

function validFollowing(item) {
  return item && Number(item.id) > 0 && item.title && typeof item.title === 'object';
}

function validTask(task) {
  return task
    && typeof task.id === 'string'
    && task.id.length > 0
    && Number(task.animeId) > 0
    && Number(task.episode) > 0
    && (task.status === 'pending' || task.status === 'completed');
}

function normalizeDocument(value) {
  if (!value || typeof value !== 'object' || Number(value.version) !== SYNC_DOCUMENT_VERSION) {
    throw new Error('WebDAV 同步文件版本不受支持');
  }
  return {
    version: SYNC_DOCUMENT_VERSION,
    updatedAt: asTimestamp(value.updatedAt),
    following: Array.isArray(value.following) ? value.following.filter(validFollowing) : [],
    tasks: Array.isArray(value.tasks) ? value.tasks.filter(validTask) : [],
    followingDeletedAt: normalizeTombstones(value.followingDeletedAt),
  };
}

function documentFromState(state) {
  const metadata = ensureSyncMetadata(state);
  const following = state.following.filter(validFollowing).map(clone).sort((left, right) => left.id - right.id);
  const tasks = state.tasks.filter(validTask).map(clone).sort((left, right) => left.id.localeCompare(right.id));
  const followingDeletedAt = normalizeTombstones(metadata.followingDeletedAt);
  const timestamps = [
    ...following.map((item) => recordTimestamp(item, 'followedAt')),
    ...tasks.map((task) => recordTimestamp(task, 'createdAt')),
    ...Object.values(followingDeletedAt),
  ];
  return {
    version: SYNC_DOCUMENT_VERSION,
    updatedAt: Math.max(0, ...timestamps),
    following,
    tasks,
    followingDeletedAt,
  };
}

function comparableDocument(document) {
  const normalized = normalizeDocument(document);
  return JSON.stringify({
    following: [...normalized.following].sort((left, right) => left.id - right.id),
    tasks: [...normalized.tasks].sort((left, right) => left.id.localeCompare(right.id)),
    followingDeletedAt: Object.fromEntries(Object.entries(normalized.followingDeletedAt).sort(([left], [right]) => left.localeCompare(right))),
  });
}

function documentsEqual(left, right) {
  return comparableDocument(left) === comparableDocument(right);
}

function mergeDocumentIntoState(state, remoteValue) {
  const before = comparableDocument(documentFromState(state));
  const local = documentFromState(state);
  const remote = normalizeDocument(remoteValue);
  const tombstones = { ...local.followingDeletedAt };
  Object.entries(remote.followingDeletedAt).forEach(([id, timestamp]) => {
    tombstones[id] = Math.max(asTimestamp(tombstones[id]), asTimestamp(timestamp));
  });

  const localFollowing = new Map(local.following.map((item) => [item.id, item]));
  const remoteFollowing = new Map(remote.following.map((item) => [item.id, item]));
  const followingIds = new Set([...localFollowing.keys(), ...remoteFollowing.keys(), ...Object.keys(tombstones).map(Number)]);
  const mergedFollowing = [];
  for (const id of followingIds) {
    const deletedAt = asTimestamp(tombstones[String(id)]);
    const localCandidate = localFollowing.get(id);
    const localIntent = asTimestamp(localCandidate?.localFollowIntentAt);
    const remotePulled = asTimestamp(localCandidate?.lastPulledFromBangumiAt);
    const localRefollow = localCandidate && localIntent > 0
      && (localIntent > deletedAt || remotePulled <= 0 || localIntent > remotePulled * 1000);
    const winner = localRefollow
      ? clone(localCandidate)
      : chooseRecord(localCandidate, remoteFollowing.get(id), 'followedAt');
    const localIntentAt = asTimestamp(winner?.localFollowIntentAt);
    const remotePulledAt = asTimestamp(winner?.lastPulledFromBangumiAt);
    if (winner && localIntentAt > 0
      && (localIntentAt > deletedAt || remotePulledAt <= 0 || localIntentAt > remotePulledAt * 1000)) {
      // A local re-follow is a fresh user intent.  Do not let an older
      // WebDAV tombstone erase it; dropping the tombstone also makes the
      // desktop fallback implementation converge with Rust/Android.
      delete tombstones[String(id)];
      mergedFollowing.push(winner);
    } else if (winner && recordTimestamp(winner, 'followedAt') > deletedAt) {
      mergedFollowing.push(winner);
    }
  }
  mergedFollowing.sort((left, right) => left.id - right.id);

  const localTasks = new Map(local.tasks.map((task) => [task.id, task]));
  const remoteTasks = new Map(remote.tasks.map((task) => [task.id, task]));
  const taskIds = new Set([...localTasks.keys(), ...remoteTasks.keys()]);
  const followedById = new Map(mergedFollowing.map((item) => [item.id, item]));
  const mergedTasks = [];
  for (const id of taskIds) {
    const winner = chooseRecord(localTasks.get(id), remoteTasks.get(id), 'createdAt');
    if (!winner) continue;
    const followed = followedById.get(winner.animeId);
    if (!followed && winner.status === 'pending') continue;
    if (followed) winner.animeTitle = followed.displayTitle;
    mergedTasks.push(winner);
  }
  mergedTasks.sort((left, right) => right.airingAt - left.airingAt || left.id.localeCompare(right.id));

  state.following = mergedFollowing;
  state.tasks = mergedTasks;
  ensureSyncMetadata(state).followingDeletedAt = tombstones;
  const merged = documentFromState(state);
  return {
    changed: before !== comparableDocument(merged),
    document: merged,
    remoteChanged: !documentsEqual(remote, merged),
  };
}

module.exports = {
  SYNC_DOCUMENT_VERSION,
  documentFromState,
  documentsEqual,
  ensureSyncMetadata,
  markFollowingChanged,
  markFollowingDeleted,
  markTaskChanged,
  mergeDocumentIntoState,
  normalizeDocument,
};
