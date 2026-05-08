import { describe, expect, it } from "vitest";

import {
  LOCAL_REMOTE_ID,
  createBuiltinLocalRemote,
  isLocalRemoteId,
  isLocalSessionRemote,
  normalizeRemoteConfigs,
  remoteConnectionLabel,
  resolveProjectRemoteId,
  resolveSessionRemoteId,
} from "./remotes";

describe("remotes", () => {
  it("normalizes missing or duplicate remote config entries", () => {
    expect(
      normalizeRemoteConfigs([
        createBuiltinLocalRemote(),
        {
          id: "ssh-lab",
          name: "SSH Lab",
          transport: "ssh",
          enabled: true,
          host: "example.com",
          port: 2222,
          user: "alice",
        },
        {
          id: "ssh-lab",
          name: "Duplicate",
          transport: "ssh",
          enabled: true,
          host: "dup.example.com",
        },
      ]),
    ).toEqual([
      createBuiltinLocalRemote(),
      {
        id: "ssh-lab",
        name: "SSH Lab",
        transport: "ssh",
        enabled: true,
        host: "example.com",
        port: 2222,
        user: "alice",
      },
    ]);
  });

  it("treats empty project remote ids as local", () => {
    expect(resolveProjectRemoteId()).toBe(LOCAL_REMOTE_ID);
    expect(resolveProjectRemoteId({ remoteId: null })).toBe(LOCAL_REMOTE_ID);
    expect(resolveProjectRemoteId({ remoteId: "" })).toBe(LOCAL_REMOTE_ID);
    expect(resolveProjectRemoteId({ remoteId: "ssh-lab" })).toBe("ssh-lab");
    expect(isLocalRemoteId()).toBe(true);
    expect(isLocalRemoteId("local")).toBe(true);
    expect(isLocalRemoteId("ssh-lab")).toBe(false);
  });

  it.each([
    [{ remoteId: "ssh-lab" }, { remoteId: "local" }, "ssh-lab"],
    [{ remoteId: "" }, { remoteId: "ssh-lab" }, "ssh-lab"],
    [{}, { remoteId: "ssh-project" }, "ssh-project"],
    [undefined, undefined, LOCAL_REMOTE_ID],
  ] as const)(
    "resolves session remote ownership for case %#",
    (session, project, expected) => {
      expect(resolveSessionRemoteId(session, project)).toBe(expected);
    },
  );

  it.each([
    [{ remoteId: "ssh-lab" }, { remoteId: "local" }, false],
    [{}, { remoteId: "local" }, true],
  ] as const)(
    "detects local session remote ownership for case %#",
    (session, project, expected) => {
      expect(isLocalSessionRemote(session, project)).toBe(expected);
    },
  );

  it("describes local and ssh connection labels", () => {
    expect(remoteConnectionLabel(createBuiltinLocalRemote())).toBe("This machine");
    expect(
      remoteConnectionLabel({
        transport: "ssh",
        host: "example.com",
        port: 2222,
        user: "alice",
      }),
    ).toBe("alice@example.com:2222");
  });
});
