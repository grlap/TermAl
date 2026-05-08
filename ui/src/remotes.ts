import type { Project, RemoteConfig, Session } from "./types";

export const LOCAL_REMOTE_ID = "local";
export const DEFAULT_SSH_REMOTE_PORT = 22;

export function createBuiltinLocalRemote(): RemoteConfig {
  return {
    id: LOCAL_REMOTE_ID,
    name: "Local",
    transport: "local",
    enabled: true,
    host: null,
    port: null,
    user: null,
  };
}

export function isLocalRemoteId(remoteId?: string | null): boolean {
  const normalized = remoteId?.trim() ?? "";
  return normalized.length === 0 || normalized === LOCAL_REMOTE_ID;
}

export function resolveProjectRemoteId(project?: Pick<Project, "remoteId"> | null): string {
  const remoteId = project?.remoteId?.trim();
  return isLocalRemoteId(remoteId) ? LOCAL_REMOTE_ID : remoteId!;
}

export function resolveSessionRemoteId(
  session?: Pick<Session, "remoteId"> | null,
  project?: Pick<Project, "remoteId"> | null,
): string {
  const remoteId = session?.remoteId ?? project?.remoteId;
  const normalized = remoteId?.trim();
  return isLocalRemoteId(normalized) ? LOCAL_REMOTE_ID : normalized!;
}

export function isLocalSessionRemote(
  session?: Pick<Session, "remoteId"> | null,
  project?: Pick<Project, "remoteId"> | null,
): boolean {
  return isLocalRemoteId(resolveSessionRemoteId(session, project));
}

export function normalizeRemoteConfigs(remotes?: RemoteConfig[] | null): RemoteConfig[] {
  const normalized: RemoteConfig[] = [createBuiltinLocalRemote()];
  const seen = new Set([LOCAL_REMOTE_ID]);

  for (const remote of remotes ?? []) {
    const id = remote.id.trim();
    const dedupeKey = id.toLowerCase();
    if (!id || dedupeKey === LOCAL_REMOTE_ID || seen.has(dedupeKey)) {
      continue;
    }

    seen.add(dedupeKey);
    normalized.push({
      id,
      name: remote.name?.trim() || id,
      transport: "ssh",
      enabled: remote.enabled !== false,
      host: remote.host?.trim() || null,
      port: typeof remote.port === "number" && Number.isFinite(remote.port) ? remote.port : null,
      user: remote.user?.trim() || null,
    });
  }

  return normalized;
}

export function remoteDisplayName(
  remote?: Pick<RemoteConfig, "id" | "name"> | null,
  fallbackId?: string | null,
): string {
  const name = remote?.name?.trim();
  if (name) {
    return name;
  }

  const remoteId = fallbackId?.trim() || remote?.id?.trim();
  return remoteId || LOCAL_REMOTE_ID;
}

export function remoteConnectionLabel(
  remote?: Pick<RemoteConfig, "transport" | "host" | "port" | "user"> | null,
): string {
  if (!remote || remote.transport === "local") {
    return "This machine";
  }

  const host = remote.host?.trim() || "Host required";
  const user = remote.user?.trim();
  const target = user ? `${user}@${host}` : host;
  const port = remote.port ?? null;
  const portSuffix = port && port !== DEFAULT_SSH_REMOTE_PORT ? `:${port}` : "";
  return `${target}${portSuffix}`;
}
