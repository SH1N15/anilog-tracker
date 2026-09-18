export interface StateRefreshController {
  refresh(reportError?: boolean): Promise<void>;
  dispose(): void;
}

export function createStateRefreshController<T>({
  getState,
  subscribe,
  applyState,
  onError,
  retryDelaysMs = [200, 500, 1000, 2000],
}: {
  getState: () => Promise<T>;
  subscribe: (callback: (state: T) => void) => () => void;
  applyState: (state: T) => void;
  onError?: (reason: unknown) => void;
  retryDelaysMs?: readonly number[];
}): StateRefreshController {
  let active = true;
  let hasSnapshot = false;
  let pushedRevision = 0;
  let refreshInFlight: Promise<void> | null = null;
  let retryTimer: ReturnType<typeof setTimeout> | undefined;
  let finishRetryWait: (() => void) | undefined;

  function cancelRetryWait() {
    if (retryTimer !== undefined) clearTimeout(retryTimer);
    retryTimer = undefined;
    const finish = finishRetryWait;
    finishRetryWait = undefined;
    finish?.();
  }

  const unsubscribe = subscribe((nextState) => {
    pushedRevision += 1;
    if (active) {
      hasSnapshot = true;
      applyState(nextState);
    }
    cancelRetryWait();
  });

  function refresh(reportError = false): Promise<void> {
    if (!active) return Promise.resolve();
    if (refreshInFlight) return refreshInFlight;
    const revisionAtStart = pushedRevision;
    refreshInFlight = (async () => {
      for (let attempt = 0; active && revisionAtStart === pushedRevision; attempt++) {
        try {
          const nextState = await getState();
          if (active && revisionAtStart === pushedRevision) {
            hasSnapshot = true;
            applyState(nextState);
          }
          return;
        } catch (reason) {
          if (!active || revisionAtStart !== pushedRevision) return;
          const delay = retryDelaysMs[attempt];
          // Only bootstrap retries. A resume failure retains the last snapshot.
          if (hasSnapshot || delay === undefined) {
            if (reportError) onError?.(reason);
            return;
          }
          await new Promise<void>((resolve) => {
            finishRetryWait = resolve;
            retryTimer = setTimeout(cancelRetryWait, delay);
          });
        }
      }
    })()
      .finally(() => { refreshInFlight = null; });
    return refreshInFlight;
  }

  return {
    refresh,
    dispose() {
      if (!active) return;
      active = false;
      cancelRetryWait();
      unsubscribe();
    },
  };
}
