import type { WatchTask } from './types';

export function needsHistoryReview(task: WatchTask): boolean {
  return task.status === 'completed' && task.completionReview?.decision === 'review';
}

export function isScheduledHistoryReset(task: WatchTask, now = Math.floor(Date.now() / 1000)): boolean {
  if (task.status !== 'pending' || task.completionReview?.decision !== 'reset' || task.airingAt <= 0) return false;
  if (task.airingPrecision === 'instant') return task.airingAt > now;
  if (task.airingPrecision === 'date') return Math.floor(task.airingAt / 86400) > Math.floor(now / 86400);
  return false;
}

export function isCompletedHistory(task: WatchTask): boolean {
  return task.status === 'completed' && !needsHistoryReview(task);
}

export function isPendingHistory(task: WatchTask): boolean {
  return task.status === 'pending' && !isScheduledHistoryReset(task);
}
