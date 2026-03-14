import "@testing-library/jest-dom";

if (!globalThis.crypto) {
  Object.defineProperty(globalThis, "crypto", {
    value: {},
    configurable: true,
  });
}

if (typeof globalThis.crypto.randomUUID !== "function") {
  Object.defineProperty(globalThis.crypto, "randomUUID", {
    value: () => `test-${Math.random().toString(16).slice(2)}`,
    configurable: true,
  });
}

if (typeof document !== "undefined" && typeof document.queryCommandSupported !== "function") {
  Object.defineProperty(document, "queryCommandSupported", {
    value: () => false,
    configurable: true,
  });
}
