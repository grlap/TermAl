import type { Session } from "./types";

export function matchingSessionModelOption(
  modelOptions: readonly NonNullable<Session["modelOptions"]>[number][] | undefined,
  requestedModel: string,
) {
  const trimmedModel = requestedModel.trim();
  if (!trimmedModel) {
    return null;
  }

  const normalizedRequestedModel = trimmedModel.toLowerCase();
  return (
    modelOptions?.find((option) => {
      const normalizedValue = option.value.trim().toLowerCase();
      const normalizedLabel = option.label.trim().toLowerCase();
      return (
        normalizedValue === normalizedRequestedModel ||
        normalizedLabel === normalizedRequestedModel
      );
    }) ?? null
  );
}
