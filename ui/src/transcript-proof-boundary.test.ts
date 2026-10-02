// Source inventory pins the private local-tail proof and HTTP effect doors.
/// <reference types="vite/client" />
import ts from "typescript";
import { expect, it } from "vitest";
const sources = import.meta.glob<string>(["./**/*.ts", "./**/*.tsx"], {
  eager: true, query: "?raw", import: "default",
});
const production = Object.entries(sources).filter(([path]) =>
  !/\.test\.|test-support|test-fixtures|test-harness/.test(path));

it("T2 has exactly two private certificate writers and no extra proof/read registries", () => {
  const writers: string[] = [], violations: string[] = [];
  for (const [path, body] of production) {
    const source = ts.createSourceFile(path, body, ts.ScriptTarget.Latest, true);
    function inspect(node: ts.Node) {
      if (ts.isBinaryExpression(node) && node.operatorToken.kind === ts.SyntaxKind.EqualsToken) {
        const left = node.left.getText(source), right = node.right.getText(source);
        if (/\.appliedSeq$/.test(left) && right !== "undefined") {
          if (path !== "./transcript-repair-authority.ts") violations.push(path + ":" + left);
          let parent: ts.Node | undefined = node.parent;
          while (parent && !ts.isMethodDeclaration(parent)) parent = parent.parent;
          writers.push(parent && ts.isMethodDeclaration(parent) ? parent.name.getText(source) : "unknown");
        }
      }
      if (ts.isPropertyAssignment(node) && ["bodySeq", "bodySeqEpoch"].includes(node.name.getText(source)) &&
          path !== "./transcript-repair-authority.ts") violations.push(path + ":proof field");
      ts.forEachChild(node, inspect);
    }
    inspect(source);
  }
  expect(violations).toEqual([]);
  expect(writers.sort()).toEqual(["receiveBodyDelta", "replayTail"]);
  const owner = sources["./transcript-repair-authority.ts"];
  expect(owner).not.toMatch(/certifyingReads|replacingReads|rangeReads|pendingPages|verificationListeners|leftEpoch|SessionReadToken|beginRead|finishRead/);
  expect(owner).toContain("needsTailRead(id: string, visible: boolean)");
  expect(owner).toMatch(/private tails = new Map/);
});

it("T2 inventories every HTTP session-read callback, not only tail adoption", () => {
  const hook = ts.createSourceFile("hook", sources["./app-live-state.ts"], ts.ScriptTarget.Latest, true);
  const callbacks = new Set(["adoptFetchedSession", "startSessionHydration",
    "createSessionHistoryLoadingContext", "publishHistorySession"]);
  const inspected: string[] = [], writes: string[] = [];
  function inspectBody(node: ts.Node) {
    if (ts.isBinaryExpression(node) && node.operatorToken.kind === ts.SyntaxKind.EqualsToken &&
        /latestStateRevisionRef|lastSeenServerInstanceIdRef|seenServerInstanceIdsRef/.test(node.left.getText(hook))) {
      writes.push(node.left.getText(hook));
    }
    if (ts.isCallExpression(node) && node.expression.getText(hook) === "rememberServerInstanceId") writes.push("remember");
    ts.forEachChild(node, inspectBody);
  }
  function visit(node: ts.Node) {
    if (ts.isFunctionDeclaration(node) && node.name && callbacks.has(node.name.text)) {
      inspected.push(node.name.text); inspectBody(node);
    }
    ts.forEachChild(node, visit);
  }
  visit(hook);
  expect(inspected.sort()).toEqual([...callbacks].sort());
  expect(writes).toEqual([]);
  expect(sources["./session-history-loading.ts"]).not.toMatch(/latestStateRevisionRef|lastSeenServerInstanceIdRef|seenServerInstanceIdsRef/);
  expect(sources["./transcript-http-adoption.ts"]).toContain("!input.pairedLocalTail");
  expect(sources["./transcript-http-adoption.ts"]).toContain("refs.latestRevision.current = decision.revision.value");
});

it("T2 permits ordinary master page merges but they never write an owner certificate", () => {
  for (const path of ["./session-history.ts", "./session-history-loading.ts"]) {
    expect(sources[path]).not.toMatch(/appliedSeq|bodyCertificate|replayTail|bodySeq\s*:/);
  }
  expect(sources["./app-live-state.ts"]).not.toMatch(/certifyingReads|certificationAttempts|readLostResidentRange|hasReplacingRead|certifyResident/);
});

it("T2 owner-origin hydration has one requester entry and one existing-interval backstop", () => {
  const entries: string[] = [];
  for (const [path, body] of production) {
    const source = ts.createSourceFile(path, body, ts.ScriptTarget.Latest, true);
    function visit(node: ts.Node) {
      if (ts.isPropertyAssignment(node) && node.name.getText(source) === "ownerTailRead" &&
        node.initializer.kind === ts.SyntaxKind.TrueKeyword) {
        let parent: ts.Node | undefined = node.parent;
        while (parent && !ts.isFunctionDeclaration(parent)) parent = parent.parent;
        entries.push(path + ":" + (parent && ts.isFunctionDeclaration(parent) ? parent.name?.text : "unknown"));
      }
      ts.forEachChild(node, visit);
    }
    visit(source);
  }
  expect(entries).toEqual(["./app-live-state.ts:requestSessionTailRead"]);
  const transport = ts.createSourceFile("transport", sources["./app-live-state-transport.ts"], ts.ScriptTarget.Latest, true);
  const backstops: ts.Node[] = [];
  function visit(node: ts.Node) {
    if (ts.isCallExpression(node) && node.expression.getText(transport) === "window.setInterval" &&
      node.arguments[1]?.getText(transport) === "LIVE_SESSION_RESUME_WATCHDOG_INTERVAL_MS") backstops.push(node.arguments[0]);
    ts.forEachChild(node, visit);
  }
  visit(transport);
  expect(backstops).toHaveLength(1);
  const body = backstops[0].getText(transport);
  expect(body).toContain("params.visibleHydrationSessionIdsRef.current");
  expect(body).toContain("params.requestSessionTailRead(id)");
  expect(body).not.toMatch(/startSessionHydration|fetchSessionTail|Date\.now/);
  expect(body.indexOf("params.requestSessionTailRead(id)")).toBeLessThan(body.indexOf("handleLiveSessionResumeWatchdogTick()"));
});
