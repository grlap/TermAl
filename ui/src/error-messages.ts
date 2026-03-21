const REMOTE_NAME_PATTERN = /remote `([^`]+)`/i;

const LOCAL_SSH_START_MARKERS = [
  "failed to start ssh connection for remote `",
];

const REMOTE_CONNECTION_MARKERS = [
  "failed to reach remote `",
  "failed to contact remote `",
  "managed start failed",
  "tunnel-only fallback failed",
];

const REMOTE_CONNECTION_DETAIL_MARKERS = [
  "ssh:",
  "could not resolve hostname",
  "permission denied",
  "connection refused",
  "connection reset",
  "name or service not known",
  "network is unreachable",
  "no route to host",
  "timed out",
  "host key verification failed",
];

function readRemoteName(message: string) {
  return REMOTE_NAME_PATTERN.exec(message)?.[1] ?? null;
}

function isLocalSshStartIssue(message: string) {
  const normalized = message.trim().toLowerCase();
  if (normalized.length === 0) {
    return false;
  }

  return LOCAL_SSH_START_MARKERS.some((marker) => normalized.includes(marker));
}

function isRemoteConnectionIssue(message: string) {
  const normalized = message.trim().toLowerCase();
  if (normalized.length === 0) {
    return false;
  }

  if (REMOTE_CONNECTION_MARKERS.some((marker) => normalized.includes(marker))) {
    return true;
  }

  if (!readRemoteName(message)) {
    return false;
  }

  return REMOTE_CONNECTION_DETAIL_MARKERS.some((marker) => normalized.includes(marker));
}

export function sanitizeUserFacingErrorMessage(message: string) {
  const normalized = message.trim();
  if (normalized.length === 0) {
    return "The request failed.";
  }

  if (isLocalSshStartIssue(normalized)) {
    const remoteName = readRemoteName(normalized);
    if (remoteName) {
      return `Could not start the local SSH client for remote "${remoteName}". Verify OpenSSH is installed and available on PATH, then try again.`;
    }

    return "Could not start the local SSH client. Verify OpenSSH is installed and available on PATH, then try again.";
  }

  if (!isRemoteConnectionIssue(normalized)) {
    return normalized;
  }

  const remoteName = readRemoteName(normalized);
  if (remoteName) {
    return `Could not connect to remote "${remoteName}" over SSH. Check the host, network, and SSH settings, then try again.`;
  }

  return "Could not connect to the remote over SSH. Check the host, network, and SSH settings, then try again.";
}

export function formatUserFacingError(error: unknown) {
  if (error instanceof Error) {
    return sanitizeUserFacingErrorMessage(error.message);
  }

  return "The request failed.";
}
