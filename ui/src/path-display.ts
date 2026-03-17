export function relativizePathToWorkspace(path: string, workspaceRoot: string | null) {
  const trimmedPath = path.trim();
  const trimmedRoot = workspaceRoot?.trim();
  if (!trimmedPath || !trimmedRoot) {
    return trimmedPath;
  }

  const normalizedPath = normalizeDisplayPath(trimmedPath);
  const normalizedRoot = normalizeDisplayPath(trimmedRoot).replace(/\/+$/, "");
  const exactPrefix = `${normalizedRoot}/`;
  if (normalizedPath.startsWith(exactPrefix)) {
    return normalizedPath.slice(exactPrefix.length);
  }

  if (looksLikeWindowsPath(trimmedPath) || looksLikeWindowsPath(trimmedRoot)) {
    const lowerPath = normalizedPath.toLowerCase();
    const lowerRoot = normalizedRoot.toLowerCase();
    const lowerPrefix = `${lowerRoot}/`;
    if (lowerPath.startsWith(lowerPrefix)) {
      return normalizedPath.slice(lowerPrefix.length);
    }
  }

  return trimmedPath;
}

export function normalizeDisplayPath(path: string) {
  return path.trim().replace(/[\\/]+/g, "/");
}

export function looksLikeWindowsPath(path: string) {
  return /^[A-Za-z]:[\\/]/.test(path) || path.includes("\\");
}

export function looksLikeAbsoluteDisplayPath(path: string) {
  return path.startsWith("/") || /^[A-Za-z]:[\\/]/.test(path) || /^\\\\/.test(path);
}
