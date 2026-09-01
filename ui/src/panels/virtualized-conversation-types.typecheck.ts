// Compile-only contract for the user-input submit chain (`.typecheck.ts`
// convention: nothing here runs). `cd ui && npx tsc --noEmit` is the
// enforcing gate — Vitest intentionally does not execute this file, and it
// exports nothing usable at runtime. tsc fails the build if either handler
// alias stops returning `Promise<void>` or if a wrapper with the exact bound
// handler parameters can legally drop that promise. The card's in-flight
// guard depends on the promise surviving every binding layer.

import type {
  BoundUserInputSubmitHandler,
  UserInputSubmitHandler,
} from "./virtualized-conversation-types";

type IsExactly<Left, Right> = [Left] extends [Right]
  ? [Right] extends [Left]
    ? true
    : false
  : false;

type Assert<Condition extends true> = Condition;

type UserInputSubmitHandlerReturnsPromise = Assert<
  IsExactly<ReturnType<UserInputSubmitHandler>, Promise<void>>
>;

type BoundUserInputSubmitHandlerReturnsPromise = Assert<
  IsExactly<ReturnType<BoundUserInputSubmitHandler>, Promise<void>>
>;

type VoidReturningBoundWrapper = (
  ...args: Parameters<BoundUserInputSubmitHandler>
) => void;

// Pin only the return-value contract. Deriving the parameters from the real
// handler prevents unrelated signature changes from satisfying this probe.
type BoundHandlerRejectsVoidWrapper = Assert<
  IsExactly<
    VoidReturningBoundWrapper extends BoundUserInputSubmitHandler ? true : false,
    false
  >
>;

// Keep the type-level assertions and negative probe referenced without
// exporting runtime API. This also keeps the file ready if the project later
// enables TypeScript's `noUnusedLocals` gate; it is not enabled today.
type Contract = [
  UserInputSubmitHandlerReturnsPromise,
  BoundUserInputSubmitHandlerReturnsPromise,
  BoundHandlerRejectsVoidWrapper,
];
void (null as unknown as Contract);

export {};
