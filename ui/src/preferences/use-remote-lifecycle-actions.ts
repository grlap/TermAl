import { useEffect, useRef, useState } from "react";

import {
  registerRemoteTermal,
  upgradeRemoteTermal,
  type RemoteActionResponse,
} from "../api";
import { formatUserFacingError } from "../error-messages";
import type { RemoteConfig } from "../types";

const DEFAULT_REMOTE_TERMAL_SOURCE_PATH = "~/src/TermAl";

export type RemoteActionState = {
  kind: "register" | "upgrade";
  status: "pending" | "success" | "error";
  message: string;
  output?: string;
};

function remoteActionSuccessMessage(response: RemoteActionResponse) {
  const output = [response.stdout, response.stderr].filter(Boolean).join("\n").trim();
  return {
    message: response.message,
    output: output || undefined,
  };
}

export function remoteLifecycleKey(remote: RemoteConfig) {
  return remote.id.trim();
}

export function useRemoteLifecycleActions() {
  const [remoteSourcePaths, setRemoteSourcePaths] = useState<Record<string, string>>({});
  const [remoteActionStates, setRemoteActionStates] = useState<Record<string, RemoteActionState>>(
    {},
  );
  const mountedRef = useRef(false);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  function remoteSourcePath(remote: RemoteConfig) {
    return remoteSourcePaths[remoteLifecycleKey(remote)] ?? DEFAULT_REMOTE_TERMAL_SOURCE_PATH;
  }

  function setRemoteSourcePath(remote: RemoteConfig, value: string) {
    setRemoteSourcePaths((current) => ({
      ...current,
      [remoteLifecycleKey(remote)]: value,
    }));
  }

  async function registerRemote(remote: RemoteConfig) {
    const remoteId = remoteLifecycleKey(remote);
    const sourcePath = remoteSourcePath(remote).trim();
    setRemoteActionStates((current) => ({
      ...current,
      [remoteId]: {
        kind: "register",
        status: "pending",
        message: "Registering remote TermAl checkout...",
      },
    }));
    try {
      const response = await registerRemoteTermal(remoteId, { sourcePath });
      if (!mountedRef.current) {
        return;
      }
      const success = remoteActionSuccessMessage(response);
      setRemoteActionStates((current) => ({
        ...current,
        [remoteId]: {
          kind: "register",
          status: "success",
          message: success.message,
          output: success.output,
        },
      }));
    } catch (error) {
      if (!mountedRef.current) {
        return;
      }
      setRemoteActionStates((current) => ({
        ...current,
        [remoteId]: {
          kind: "register",
          status: "error",
          message: formatUserFacingError(error),
        },
      }));
    }
  }

  async function upgradeRemote(remote: RemoteConfig) {
    const remoteId = remoteLifecycleKey(remote);
    setRemoteActionStates((current) => ({
      ...current,
      [remoteId]: {
        kind: "upgrade",
        status: "pending",
        message: "Building and installing TermAl on remote...",
      },
    }));
    try {
      const response = await upgradeRemoteTermal(remoteId);
      if (!mountedRef.current) {
        return;
      }
      const success = remoteActionSuccessMessage(response);
      setRemoteActionStates((current) => ({
        ...current,
        [remoteId]: {
          kind: "upgrade",
          status: "success",
          message: success.message,
          output: success.output,
        },
      }));
    } catch (error) {
      if (!mountedRef.current) {
        return;
      }
      setRemoteActionStates((current) => ({
        ...current,
        [remoteId]: {
          kind: "upgrade",
          status: "error",
          message: formatUserFacingError(error),
        },
      }));
    }
  }

  return {
    remoteActionStates,
    remoteLifecycleKey,
    remoteSourcePath,
    registerRemote,
    setRemoteSourcePath,
    upgradeRemote,
  };
}
