// Source and ordinary operation budgets, shared with their compiled host owners.
import { readFileSync } from "node:fs";

// The same fixed pair is compiled by the host: minimum, process delivery.
export const [engramOperationBudgetMs, sourceProcessDeliveryMarginMs] = JSON.parse(readFileSync(
  new URL("./engram-call-budget-ms.json", import.meta.url), "utf8"));
export const sourceIdBudgetMs = JSON.parse(readFileSync(
  new URL("./source-id-budget-ms.json", import.meta.url), "utf8"));
// Outer transports reserve time for process startup/envelope delivery or HTTP.
const metadataHttpMarginMs = 5_000;
export const sourceProcessBudgetMs = Math.max(sourceIdBudgetMs, engramOperationBudgetMs) + sourceProcessDeliveryMarginMs;
export const sourceMetadataBudgetMs = sourceProcessBudgetMs * 2;
export const sourceMetadataHttpBudgetMs = sourceMetadataBudgetMs + metadataHttpMarginMs;

// Diagnostic only: clearing this timer never cancels the operation it names.
export function warnSlowEngramOperation(operation) {
  const started = Date.now();
  const timer = setTimeout(() => {
    console.error(`engram warning> ${operation} still running after ${Date.now() - started} ms`);
  }, 10_000);
  timer.unref?.();
  return timer;
}
