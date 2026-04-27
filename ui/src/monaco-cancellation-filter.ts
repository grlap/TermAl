const INSTALL_MARKER = "__termalMonacoCancellationFilterInstalled";

type RejectionTarget = {
  addEventListener?: Window["addEventListener"];
  [INSTALL_MARKER]?: boolean;
};

export function installMonacoCancellationRejectionFilter(
  target: RejectionTarget = defaultRejectionTarget(),
) {
  if (target[INSTALL_MARKER] || typeof target.addEventListener !== "function") {
    return;
  }

  target[INSTALL_MARKER] = true;
  target.addEventListener("unhandledrejection", (event) => {
    if (isBenignMonacoCancellationReason(event.reason)) {
      event.preventDefault();
    }
  });
}

function defaultRejectionTarget(): RejectionTarget {
  return typeof window === "undefined" ? {} : window;
}

export function isBenignMonacoCancellationReason(reason: unknown) {
  if (!isCanceledError(reason)) {
    return false;
  }

  const stack = typeof reason.stack === "string" ? reason.stack : "";
  const hasMonacoWorkerFrame =
    stack.includes("EditorWorkerClient") ||
    stack.includes("StandaloneEditorWorkerService") ||
    stack.includes("workerWithSyncedResources");
  return hasMonacoWorkerFrame && stack.includes("computeDiff");
}

function isCanceledError(reason: unknown): reason is Error {
  return (
    reason instanceof Error &&
    reason.name === "Canceled" &&
    reason.message === "Canceled"
  );
}
