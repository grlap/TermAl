// Owns failed wait-snapshot repair retries, independently of SSE reconnect
// confirmation. Extracted from app-live-state-transport.ts; the transport owns
// fetching, snapshot adoption, and coalescing with other recovery requests.
// Retain only wait intent; navigation and live-connection proof are one-shot.
import {
  coalescePendingStateResyncOptions,
  type PendingStateResyncOptions,
  type RequestStateResyncOptions,
} from "./app-live-state-resync-options";
import { RECONNECT_STATE_RESYNC_DELAY_MS, RECONNECT_STATE_RESYNC_MAX_DELAY_MS } from "./app-shell-internals";

function requestOptions(options: PendingStateResyncOptions): RequestStateResyncOptions {
  return {
    allowSameServerEqualRevision: true,
    waitRepair: options.waitRepair ?? undefined,
  };
}

export function createWaitSnapshotRepairRetry(
  request: (options: RequestStateResyncOptions) => void,
  isOnline: () => boolean,
) {
  let timer: ReturnType<typeof window.setTimeout> | null = null;
  let pending: PendingStateResyncOptions | null = null;
  let delay = RECONNECT_STATE_RESYNC_DELAY_MS;
  let disposed = false;

  function take(): RequestStateResyncOptions | undefined {
    if (timer !== null) window.clearTimeout(timer);
    timer = null;
    const options = pending;
    pending = null;
    return options ? requestOptions(options) : undefined;
  }

  function arm() {
    if (disposed || timer !== null || pending === null) return;
    timer = window.setTimeout(() => {
      timer = null;
      if (disposed) return;
      if (!isOnline()) {
        arm();
        return;
      }
      const options = take();
      if (options) request(options);
    }, delay);
    delay = Math.min(delay * 2, RECONNECT_STATE_RESYNC_MAX_DELAY_MS);
  }

  return {
    take,
    schedule(options: PendingStateResyncOptions) {
      if (!options.waitRepair || disposed) return;
      pending = coalescePendingStateResyncOptions(pending, requestOptions(options));
      arm();
    },
    complete() {
      take();
      delay = RECONNECT_STATE_RESYNC_DELAY_MS;
    },
    dispose() {
      disposed = true;
      take();
    },
  };
}
