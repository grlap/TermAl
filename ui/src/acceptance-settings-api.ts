// Owns project acceptance settings HTTP contracts. No task evaluation or enablement.
import { request } from "./api-request";
import type { StateResponse } from "./api";
import type { AcceptanceEvaluationMode, EvaluatorDefaults } from "./types";
export type { EvaluatorDefaults } from "./types";
export type StoreAcceptancePolicy = {
  modes: AcceptanceEvaluationMode[];
  mechanicalBasis: "asserted" | "observed";
  requireSourceFreshness: boolean;
};
export type AcceptancePolicySnapshot = {
  writeApplied?: boolean;
  available: boolean;
  error?: string;
  readerKey: string;
  policy?: string;
  epoch?: number;
  requiredAssurance?: string;
  acceptanceEvaluation?: StoreAcceptancePolicy;
};
export type PolicyChange = StoreAcceptancePolicy & {
  expectedPolicy: string;
  readerKey: string;
  idempotencyKey: string;
};
const path = (id: string) => `/api/projects/${encodeURIComponent(id)}/engram`;
export const getAcceptancePolicy = (id: string, signal?: AbortSignal) =>
  request<AcceptancePolicySnapshot>(`${path(id)}/control-policy`, { signal }, { preserveGatewayErrorBody: true });
export const saveEvaluatorDefaults = (id: string, value: EvaluatorDefaults) =>
  request<StateResponse>(`${path(id)}/acceptance-evaluation-defaults`, {
    method: "PATCH", headers: { "Content-Type": "application/json" }, body: JSON.stringify(value),
  });
export const changeAcceptancePolicy = (id: string, value: PolicyChange) =>
  request<AcceptancePolicySnapshot>(`${path(id)}/acceptance-evaluation-policy`, {
    method: "POST", headers: { "Content-Type": "application/json", "X-TermAl-Operator-Action": "acceptance-policy" },
    body: JSON.stringify(value),
  }, { preserveGatewayErrorBody: true });
