// delegation-result-types.ts
//
// Shared result-packet shapes used by delegation transports and pure prompt
// formatting. Keep this free of route/client imports.

import type {
  DelegationCommandResult,
  DelegationFinding,
  DelegationStatus,
} from "./types";

export type DelegationResultPacket = {
  delegationId: string;
  childSessionId: string;
  status: DelegationStatus;
  summary: string;
  findings: DelegationFinding[];
  changedFiles: string[];
  commandsRun: DelegationCommandResult[];
  notes: string[];
  revision: number;
  serverInstanceId: string;
};
