// Owns one shared, visibility-aware minute timer, independent of panel renders.
// The last subscriber leaving releases both the timer and visibility listener.
const subscribers = new Set<() => void>();
let timestamp = Date.now();
let timer: ReturnType<typeof setTimeout> | undefined;

function clearTimer() {
  if (timer !== undefined) {
    clearTimeout(timer);
    timer = undefined;
  }
}

function refresh() {
  clearTimer();
  if (subscribers.size === 0 || document.hidden) return;

  timestamp = Date.now();
  for (const notify of subscribers) notify();

  if (subscribers.size > 0 && !document.hidden) {
    timer = setTimeout(refresh, 60_000 - (Date.now() % 60_000));
  }
}

export function subscribeSidebarMinuteClock(notify: () => void) {
  subscribers.add(notify);
  if (subscribers.size === 1) {
    timestamp = Date.now();
    document.addEventListener("visibilitychange", refresh);
    refresh();
  }
  return () => {
    subscribers.delete(notify);
    if (subscribers.size === 0) {
      clearTimer();
      document.removeEventListener("visibilitychange", refresh);
    }
  };
}

export function getSidebarMinuteSnapshot() {
  return timestamp;
}
