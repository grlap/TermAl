// Installs the fixture boundary before any App module can request data.
import { createStateEventWorkload, FIXTURE_WORKSPACE_ID } from "./state-event-test-fixtures";
import { installStateEventFixtureTransport } from "./state-event-fixture-transport";
const url = new URL(location.href);
url.searchParams.set("workspace", FIXTURE_WORKSPACE_ID);
history.replaceState(null, "", url);
// This preview has its own origin; clear only its own workspace cache.
localStorage.removeItem(`termal-workspace-layout:${FIXTURE_WORKSPACE_ID}`);
const workload = createStateEventWorkload();
const transport = installStateEventFixtureTransport(workload);
Object.assign(globalThis, { appLatencyTransport: transport });
await import("./app-latency-preview");
