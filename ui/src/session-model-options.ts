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

export function codexFastServiceTier(
  session: {
    model: string;
    modelOptions?: readonly NonNullable<Session["modelOptions"]>[number][];
  },
  requestedModel = session.model,
) {
  const capability = codexFastCapability(session, requestedModel);
  return capability.status === "supported" ? capability.tier : null;
}

export function codexFastCapability(
  session: {
    model: string;
    modelOptions?: readonly NonNullable<Session["modelOptions"]>[number][];
  },
  requestedModel = session.model,
) {
  if (!session.modelOptions?.length) {
    return { status: "unknown" as const, tier: null };
  }

  // Fast authority follows the backend's exact `codex_model_option` lookup.
  // Callers handling user-entered model changes must normalize the input to a
  // canonical catalog value before asking for capability.
  const option =
    session.modelOptions.find(
      (candidate) => candidate.value === requestedModel.trim(),
    ) ?? null;
  if (!option) {
    return { status: "unknown" as const, tier: null };
  }

  const tier = option.serviceTiers?.find(
    (candidate) =>
      candidate.label.trim().toLowerCase() === "fast" ||
      candidate.id.trim().toLowerCase() === "priority" ||
      candidate.id.trim().toLowerCase() === "fast",
  );
  return tier
    ? { status: "supported" as const, tier }
    : { status: "unsupported" as const, tier: null };
}

export function codexFastModeAfterModelChange(
  session: {
    codexFastMode?: boolean;
    model: string;
    modelOptions?: readonly NonNullable<Session["modelOptions"]>[number][];
  },
  requestedModel: string,
) {
  if (!session.codexFastMode) {
    return false;
  }

  // Mirror the backend settings contract exactly. An empty catalog means the
  // persisted authority survived a restart and may be carried until refresh.
  // Once any catalog is loaded, however, a missing target is rejected by the
  // backend just like a target that explicitly omits Fast, so the model-change
  // payload must clear Fast instead of predicting a request that will 400.
  const normalizedRequestedModel =
    matchingSessionModelOption(session.modelOptions, requestedModel)?.value ??
    requestedModel.trim();
  return (
    !session.modelOptions?.length ||
    codexFastCapability(session, normalizedRequestedModel).status === "supported"
  );
}
