import { renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { useStableMapBySignature } from "./use-stable-map-by-signature";

describe("useStableMapBySignature", () => {
  it("keeps empty map identity stable across consecutive empty renders", () => {
    const { result, rerender } = renderHook(
      ({ map }) => useStableMapBySignature(map),
      { initialProps: { map: new Map<string, string>() } },
    );

    const stableEmptyMap = result.current;

    rerender({ map: new Map<string, string>() });

    expect(result.current).toBe(stableEmptyMap);
  });

  it("keeps the previous map identity when the entry signature is unchanged", () => {
    const firstMap = new Map([
      ["message-1", "inactive"],
      ["message-2", "resolved"],
    ]);
    const { result, rerender } = renderHook(
      ({ map }) => useStableMapBySignature(map),
      { initialProps: { map: firstMap } },
    );

    const stableFirstMap = result.current;

    rerender({
      map: new Map([
        ["message-1", "inactive"],
        ["message-2", "resolved"],
      ]),
    });

    expect(result.current).toBe(stableFirstMap);
  });

  it("returns the new map when any entry changes", () => {
    const firstMap = new Map([["message-1", "inactive"]]);
    const { result, rerender } = renderHook(
      ({ map }) => useStableMapBySignature(map),
      { initialProps: { map: firstMap } },
    );

    const stableFirstMap = result.current;
    const changedMap = new Map([["message-1", "resolved"]]);

    rerender({ map: changedMap });

    expect(result.current).toBe(changedMap);
    expect(result.current).not.toBe(stableFirstMap);
  });

  it("returns the new map when one entry in a larger map changes", () => {
    const firstMap = new Map([
      ["message-1", "inactive"],
      ["message-2", "resolved"],
      ["message-3", "active"],
    ]);
    const { result, rerender } = renderHook(
      ({ map }) => useStableMapBySignature(map),
      { initialProps: { map: firstMap } },
    );

    const stableFirstMap = result.current;
    const changedValueMap = new Map([
      ["message-1", "inactive"],
      ["message-2", "active"],
      ["message-3", "active"],
    ]);
    const changedKeyMap = new Map([
      ["message-1", "inactive"],
      ["message-2b", "active"],
      ["message-3", "active"],
    ]);

    rerender({ map: changedValueMap });

    expect(result.current).toBe(changedValueMap);
    expect(result.current).not.toBe(stableFirstMap);

    rerender({ map: changedKeyMap });

    expect(result.current).toBe(changedKeyMap);
    expect(result.current).not.toBe(changedValueMap);
  });

  it("does not collide when keys or values contain signature delimiters", () => {
    const { result, rerender } = renderHook(
      ({ map }) => useStableMapBySignature(map),
      { initialProps: { map: new Map([["a:b", "c"]]) } },
    );

    const stableFirstMap = result.current;
    const changedMap = new Map([["a", "b:c"]]);

    rerender({ map: changedMap });

    expect(result.current).toBe(changedMap);
    expect(result.current).not.toBe(stableFirstMap);
  });

  it("keeps empty-string values stable across equivalent renders", () => {
    const firstMap = new Map([["message-1", ""]]);
    const { result, rerender } = renderHook(
      ({ map }) => useStableMapBySignature(map),
      { initialProps: { map: firstMap } },
    );

    const stableFirstMap = result.current;

    rerender({ map: new Map([["message-1", ""]]) });

    expect(result.current).toBe(stableFirstMap);
  });

  it("treats insertion order as part of the map signature", () => {
    const firstMap = new Map([
      ["message-1", "inactive"],
      ["message-2", "resolved"],
    ]);
    const { result, rerender } = renderHook(
      ({ map }) => useStableMapBySignature(map),
      { initialProps: { map: firstMap } },
    );

    const stableFirstMap = result.current;
    const reorderedMap = new Map([
      ["message-2", "resolved"],
      ["message-1", "inactive"],
    ]);

    rerender({ map: reorderedMap });

    expect(result.current).toBe(reorderedMap);
    expect(result.current).not.toBe(stableFirstMap);
  });

  it("uses single-slot cache semantics when a previous signature returns later", () => {
    const firstMap = new Map([["message-1", "inactive"]]);
    const { result, rerender } = renderHook(
      ({ map }) => useStableMapBySignature(map),
      { initialProps: { map: firstMap } },
    );

    const stableFirstMap = result.current;
    const changedMap = new Map([["message-1", "resolved"]]);
    const laterMatchingFirstMap = new Map([["message-1", "inactive"]]);

    rerender({ map: changedMap });
    rerender({ map: laterMatchingFirstMap });

    expect(result.current).toBe(laterMatchingFirstMap);
    expect(result.current).not.toBe(stableFirstMap);
  });
});
