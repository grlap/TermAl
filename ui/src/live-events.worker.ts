import { createSharedLiveEventHub } from "./shared-live-events";

const hub = createSharedLiveEventHub();
// Keep worker globals local to this module; adding lib.webworker globally
// would conflict with the app's DOM types.
const workerScope = self as unknown as {
  onconnect: (event: MessageEvent) => void;
};
workerScope.onconnect = (event) => {
  for (const port of event.ports) hub.connect(port);
};
