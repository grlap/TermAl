// Owns preview-only observations around the real App. No product callbacks are replaced.
import { Profiler, StrictMode } from "react";
import { createRoot } from "react-dom/client";
import App from "../App";
import "../themes/index.css";
import "../styles.css";

const commits: { at: number; phase: string; renderMilliseconds: number }[] = [];
const interactions: { name: string; startTime: number; duration: number;
  processingStart: number; processingEnd: number; interactionId: number }[] = [];
const longTasks: { startTime: number; duration: number }[] = [];
const errors: string[] = [];
window.addEventListener("error", event => errors.push(event.message));
window.addEventListener("unhandledrejection", event => errors.push(String(event.reason)));
if (PerformanceObserver.supportedEntryTypes.includes("event")) {
  new PerformanceObserver(list => {
    for (const entry of list.getEntries()) {
      const event = entry as PerformanceEventTiming & { interactionId?: number };
      if (event.interactionId) interactions.push({ name: event.name, startTime: event.startTime,
        duration: event.duration, processingStart: event.processingStart,
        processingEnd: event.processingEnd, interactionId: event.interactionId });
    }
  }).observe({ type: "event", buffered: true, durationThreshold: 16 } as PerformanceObserverInit);
}
if (PerformanceObserver.supportedEntryTypes.includes("longtask")) {
  new PerformanceObserver(list => {
    for (const entry of list.getEntries()) longTasks.push({ startTime: entry.startTime, duration: entry.duration });
  }).observe({ type: "longtask", buffered: true });
}
Object.assign(globalThis, { appLatencyObservations: { commits, interactions, longTasks, errors,
  reactProfilerAvailable: import.meta.env.DEV,
  limitations: "Synthetic finite events on the native EventSource fallback, no SharedWorker overhead or live INP equivalence. Production React Profiler samples are unavailable." } });
createRoot(document.getElementById("root")!).render(
  <StrictMode><Profiler id="actual-app" onRender={(_id, phase, renderMilliseconds) => {
    commits.push({ at: performance.now(), phase, renderMilliseconds });
  }}><App /></Profiler></StrictMode>,
);
