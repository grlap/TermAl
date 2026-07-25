import { describe, expect, it } from "vitest";

describe.each([
  ["localStorage", () => globalThis.localStorage, () => window.localStorage],
  [
    "sessionStorage",
    () => globalThis.sessionStorage,
    () => window.sessionStorage,
  ],
] as const)("%s test shim", (key, getGlobalStorage, getWindowStorage) => {
  it("is writable, shared with jsdom, and implements Storage", () => {
    const storage = getGlobalStorage();
    const descriptor = Object.getOwnPropertyDescriptor(globalThis, key);

    expect(descriptor).toMatchObject({
      configurable: true,
      writable: true,
    });
    expect(storage).toBe(getWindowStorage());

    storage.setItem("fixture", "value");
    expect(storage.length).toBe(1);
    expect(storage.key(0)).toBe("fixture");
    expect(storage.getItem("fixture")).toBe("value");
    expect(storage.fixture).toBe("value");
    expect("fixture" in storage).toBe(true);
    expect(Object.keys(storage)).toEqual(["fixture"]);

    delete storage.fixture;
    expect(storage.length).toBe(0);
    expect(storage.getItem("fixture")).toBeNull();
    expect("fixture" in storage).toBe(false);
  });

  it("maps named-property assignment to string-valued storage entries", () => {
    const storage = getGlobalStorage();

    storage.assignedFixture = 42;

    expect(storage.getItem("assignedFixture")).toBe("42");
    expect(storage.assignedFixture).toBe("42");
  });

  it("starts each test with empty storage", () => {
    const storage = getGlobalStorage();

    expect(storage.length).toBe(0);
    expect(storage.getItem("assignedFixture")).toBeNull();
  });
});
