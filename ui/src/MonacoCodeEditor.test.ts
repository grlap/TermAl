// Unit coverage for the helpers exported by `MonacoCodeEditor.tsx`
// that have no dependency on Monaco itself. The full editor is
// heavy (real Monaco via `monaco-editor/esm`), so the existing
// tests in `App.test.tsx` / `SourcePanel.test.tsx` /
// `DiffPanel.test.tsx` mock it out entirely. These tests target
// the small pure functions that live alongside the component so
// their contracts are pinned without the Monaco harness.

import { describe, expect, it } from "vitest";

import { computeInlineZoneStructureKey } from "./MonacoCodeEditor";

describe("computeInlineZoneStructureKey", () => {
  // Contract: the observer effect keyed on this string must see
  // the SAME string for structurally-equal host sets, and a
  // DIFFERENT string the moment the set changes. Any regression
  // that broke that equivalence would revert the "observer reset
  // on every keystroke" hot path that the fix specifically
  // addresses.

  it("returns the empty string for an empty host set", () => {
    expect(computeInlineZoneStructureKey([])).toBe("");
  });

  it("returns the id for a single-host set", () => {
    expect(computeInlineZoneStructureKey([{ id: "mermaid:10:20:abc" }])).toBe(
      "mermaid:10:20:abc",
    );
  });

  it("joins ids with a newline for multi-host sets", () => {
    expect(
      computeInlineZoneStructureKey([
        { id: "mermaid:1:5:aa" },
        { id: "mermaid:20:30:bb" },
        { id: "math:50:51:cc" },
      ]),
    ).toBe("mermaid:1:5:aa\nmermaid:20:30:bb\nmath:50:51:cc");
  });

  describe("structural stability", () => {
    // The load-bearing property: two different array references
    // carrying the same ids in the same order produce the SAME
    // string. JS `===`/`Object.is` on two equal-content strings
    // is `true`, so the useEffect dep passes `Object.is` → the
    // effect body is skipped. This is what stops the
    // ResizeObserver from rebuilding on every keystroke.
    it("produces the same string for structurally-equal host sets with different array refs", () => {
      const first = [
        { id: "mermaid:1:5:aa" },
        { id: "mermaid:20:30:bb" },
      ];
      const second = [
        { id: "mermaid:1:5:aa" },
        { id: "mermaid:20:30:bb" },
      ];
      expect(first).not.toBe(second); // sanity — fresh arrays
      const firstKey = computeInlineZoneStructureKey(first);
      const secondKey = computeInlineZoneStructureKey(second);
      expect(firstKey).toBe(secondKey);
      // Object.is is what React uses for dep comparison — confirm
      // equal strings satisfy it.
      expect(Object.is(firstKey, secondKey)).toBe(true);
    });

    it("produces the same string when only the objects' identities differ", () => {
      // `inlineZoneHostState` entries typically come from
      // `inlineZones.map(...)` in the zone-sync effect, which
      // creates fresh `{ id, node, zone }` objects on every
      // prop change. Even with fresh object identities, a stable
      // `id` field must yield a stable key.
      const first = { id: "zone-a" };
      const second = { id: "zone-a" };
      expect(first).not.toBe(second);
      expect(computeInlineZoneStructureKey([first])).toBe(
        computeInlineZoneStructureKey([second]),
      );
    });
  });

  describe("structural changes flip the key", () => {
    it("produces different strings when an id is added", () => {
      const before = computeInlineZoneStructureKey([{ id: "zone-a" }]);
      const after = computeInlineZoneStructureKey([
        { id: "zone-a" },
        { id: "zone-b" },
      ]);
      expect(before).not.toBe(after);
    });

    it("produces different strings when an id is removed", () => {
      const before = computeInlineZoneStructureKey([
        { id: "zone-a" },
        { id: "zone-b" },
      ]);
      const after = computeInlineZoneStructureKey([{ id: "zone-a" }]);
      expect(before).not.toBe(after);
    });

    it("produces different strings when ids are reordered", () => {
      // The separator-joined representation is order-sensitive on
      // purpose: the ResizeObserver registers each host
      // individually (via `observer.observe(host.innerNode)`), so
      // reordering doesn't matter in isolation — BUT the zone
      // registry in `inlineZoneHostsRef.current` is a Map and its
      // iteration order matches insertion order, which is the
      // same order as the `inlineZones` prop. A reorder is
      // unusual in practice (zone ids include line numbers, and
      // lines don't spontaneously shuffle), but we preserve the
      // signal rather than lose it to `[...ids].sort()`.
      const before = computeInlineZoneStructureKey([
        { id: "zone-a" },
        { id: "zone-b" },
      ]);
      const after = computeInlineZoneStructureKey([
        { id: "zone-b" },
        { id: "zone-a" },
      ]);
      expect(before).not.toBe(after);
    });

    it("produces different strings when a single id changes", () => {
      // The `mermaid:` / `mermaid-file:` id format in
      // `source-renderers.ts` includes a content hash, so any
      // edit to the fence body that actually changes the rendered
      // content produces a new id. The key must reflect that.
      const before = computeInlineZoneStructureKey([{ id: "mermaid:1:5:aa" }]);
      const after = computeInlineZoneStructureKey([{ id: "mermaid:1:5:bb" }]);
      expect(before).not.toBe(after);
    });
  });
});
