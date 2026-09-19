// delegation-result-types.ts
//
// Shared result-packet shapes used by delegation transports and pure prompt
// formatting. Keep this free of route/client imports.

import type {
  DelegationAcceptanceEvaluation,
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
  // Evaluator delegations only: what was judged and what the tracker holds.
  // The summary is the child's prose and cannot stand in for it.
  acceptanceEvaluation?: DelegationAcceptanceEvaluation;
  revision: number;
  serverInstanceId: string;
};
