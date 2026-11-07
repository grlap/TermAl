// One source observation budget, shared with the host's compiled adapter.
import { readFileSync } from "node:fs";

export const sourceIdBudgetMs = JSON.parse(readFileSync(
  new URL("./source-id-budget-ms.json", import.meta.url), "utf8"));
// Outer transports reserve time for process startup/envelope delivery or HTTP.
const processDeliveryMarginMs = 2_000;
const metadataHttpMarginMs = 5_000;
export const sourceProcessBudgetMs = sourceIdBudgetMs + processDeliveryMarginMs;
export const sourceMetadataHttpBudgetMs = sourceIdBudgetMs + metadataHttpMarginMs;
