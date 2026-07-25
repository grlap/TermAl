import "@testing-library/jest-dom";
import { beforeEach } from "vitest";

function createMemoryStorage(): Storage {
  const entries = new Map<string, string>();

  class MemoryStorage implements Storage {
    get length() {
      return entries.size;
    }

    clear() {
      entries.clear();
    }

    getItem(key: string) {
      return entries.get(String(key)) ?? null;
    }

    key(index: number) {
      return Array.from(entries.keys())[index] ?? null;
    }

    removeItem(key: string) {
      entries.delete(String(key));
    }

    setItem(key: string, value: string) {
      entries.set(String(key), String(value));
    }
  }

  const storage = new MemoryStorage();
  return new Proxy(storage, {
    deleteProperty(target, property) {
      if (typeof property === "string" && !(property in target)) {
        entries.delete(property);
        return true;
      }
      return Reflect.deleteProperty(target, property);
    },
    get(target, property) {
      if (typeof property === "string" && !(property in target)) {
        return entries.get(property);
      }
      const value = Reflect.get(target, property, target);
      return typeof value === "function" ? value.bind(target) : value;
    },
    getOwnPropertyDescriptor(target, property) {
      if (typeof property === "string" && entries.has(property)) {
        return {
          configurable: true,
          enumerable: true,
          value: entries.get(property),
          writable: true,
        };
      }
      return Reflect.getOwnPropertyDescriptor(target, property);
    },
    has(target, property) {
      return (
        Reflect.has(target, property) ||
        (typeof property === "string" && entries.has(property))
      );
    },
    ownKeys(target) {
      return [...Reflect.ownKeys(target), ...entries.keys()];
    },
    set(target, property, value) {
      if (typeof property === "string" && !(property in target)) {
        entries.set(property, String(value));
        return true;
      }
      return Reflect.set(target, property, value, target);
    },
  });
}

// Node 26 exposes disabled web-storage accessors unless the process receives
// --localstorage-file. Those accessors shadow jsdom's storage globals and turn
// ordinary App renders into undefined-property failures. Tests need isolated,
// in-memory browser storage regardless of the Node launch flags.
const testLocalStorage = createMemoryStorage();
const testSessionStorage = createMemoryStorage();

function installMemoryStorage(
  key: "localStorage" | "sessionStorage",
  storage: Storage,
) {
  const descriptor: PropertyDescriptor = {
    configurable: true,
    value: storage,
    writable: true,
  };
  Object.defineProperty(globalThis, key, descriptor);
  if (typeof window !== "undefined" && window !== globalThis) {
    Object.defineProperty(window, key, descriptor);
  }
}

function resetTestWebStorage() {
  testLocalStorage.clear();
  testSessionStorage.clear();
  installMemoryStorage("localStorage", testLocalStorage);
  installMemoryStorage("sessionStorage", testSessionStorage);
}

resetTestWebStorage();
beforeEach(resetTestWebStorage);

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

if (
  typeof HTMLElement !== "undefined" &&
  typeof HTMLElement.prototype.scrollIntoView !== "function"
) {
  Object.defineProperty(HTMLElement.prototype, "scrollIntoView", {
    value: () => {},
    configurable: true,
    writable: true,
  });
}
