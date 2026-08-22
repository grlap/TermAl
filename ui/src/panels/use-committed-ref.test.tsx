import { StrictMode, Suspense, useLayoutEffect, type ReactNode } from "react";
import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { useCommittedRef } from "./use-committed-ref";

const neverSettles = new Promise<never>(() => {});

function Probe({
  onCommit,
  suspend,
  value,
}: {
  onCommit: (readCommittedValue: () => string) => void;
  suspend: boolean;
  value: string;
}) {
  const valueRef = useCommittedRef(value);
  useLayoutEffect(() => {
    onCommit(() => valueRef.current);
  }, [onCommit, valueRef]);
  if (suspend) {
    throw neverSettles;
  }
  return <span>{value}</span>;
}

function strictWrapper({ children }: { children: ReactNode }) {
  return <StrictMode>{children}</StrictMode>;
}

describe("useCommittedRef", () => {
  it("does not publish a value from an abandoned render", () => {
    let readCommittedValue = () => "not committed";
    const onCommit = (read: () => string) => {
      readCommittedValue = read;
    };
    const view = render(
      <Suspense fallback={<span>loading</span>}>
        <Probe onCommit={onCommit} suspend={false} value="committed" />
      </Suspense>,
      { wrapper: strictWrapper },
    );
    expect(readCommittedValue()).toBe("committed");

    view.rerender(
      <Suspense fallback={<span>loading</span>}>
        <Probe onCommit={onCommit} suspend value="discarded" />
      </Suspense>,
    );

    expect(readCommittedValue()).toBe("committed");
  });

  it("publishes the latest value after a repeated StrictMode render commits", () => {
    let readCommittedValue = () => "not committed";
    const onCommit = (read: () => string) => {
      readCommittedValue = read;
    };
    const view = render(
      <Probe onCommit={onCommit} suspend={false} value="first" />,
      { wrapper: strictWrapper },
    );
    view.rerender(
      <Probe onCommit={onCommit} suspend={false} value="second" />,
    );

    expect(readCommittedValue()).toBe("second");
  });
});
