// Owns decoding a complete targeted session projection, independently of its
// bounded message window. Empty fields omitted by the wire are empty, not a
// request to retain values from a previous client copy. History pages supply
// only a window and must not be used as a replacement session projection.
import type { Session } from "./types";

export function decodeTargetedSessionProjection(wire: Session, previous?: Session): Session {
  return {
    ...wire,
    pendingPrompts: wire.pendingPrompts ?? [],
    markers: wire.markers ?? [],
    promptHistory: wire.promptHistoryRedacted === true
      ? previous?.promptHistory ?? [] : wire.promptHistory ?? [],
    promptHistoryRedacted: wire.promptHistoryRedacted === true,
    modelOptions: wire.modelOptions ?? [],
    kimiEffortOptions: wire.kimiEffortOptions ?? [],
    opencodeEffortOptions: wire.opencodeEffortOptions ?? [],
    opencodeModeOptions: wire.opencodeModeOptions ?? [],
    liveActivity: wire.liveActivity ?? null,
  };
}
