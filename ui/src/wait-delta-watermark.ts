// Owns revision ordering for one wait list. The transport admits only deltas
// from the adopted server; snapshots reach this helper only after adoption.
// A delta at R protects the whole list from snapshots below R. A snapshot at
// R covers sibling mutations too. Revisions never carry across server instances.
// Extracted from app-live-state.ts; does not own identity admission, which
// remains with app-live-state-transport-events.ts and the snapshot adoption gate.
export class WaitDeltaWatermark {
  private serverInstanceId: string | null = null;
  private revision: number | null = null;

  private useServer(serverInstanceId: string) {
    // Missing identity does not prove a restart of the known server.
    if (serverInstanceId && serverInstanceId !== this.serverInstanceId) {
      this.serverInstanceId = serverInstanceId;
      this.revision = null;
    }
  }

  acceptDelta(revision: number, serverInstanceId: string, stateRevision: number | null, created: boolean) {
    this.useServer(serverInstanceId);
    if (created && revision < Math.max(stateRevision ?? -Infinity, this.revision ?? -Infinity)) return false;
    // Consuming an unloaded wait still proves an older list is stale.
    this.revision = Math.max(this.revision ?? revision, revision);
    return true;
  }

  snapshotCovers(revision: number, serverInstanceId: string) {
    this.useServer(serverInstanceId);
    return revision >= (this.revision ?? -Infinity);
  }
}
