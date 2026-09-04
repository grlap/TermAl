// Owns composer Send, historical-tail loading and live-turn scroll integration.
// Does not own keyboard routing, tab restoration or layout clamps.
// Split from App.scroll-behavior.test.tsx; uses shared App and scroll fixtures.
import { act, cleanup, fireEvent, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import * as api from "./api";
import { getSessionRecordSnapshotForTesting } from "./session-store";
import { requestSessionHistoryAroundPage } from "./session-history-demand";
import type { Session } from "./types";
import { WORKSPACE_LAYOUT_STORAGE_KEY } from "./workspace-storage";
import {
  EventSourceMock,
  ResizeObserverMock,
  clickAndSettle,
  createDeferred,
  dispatchStateEvent,
  filterScrollToCallsAt,
  flushUiWork,
  jsonResponse,
  latestEventSource,
  makeSession,
  makeStateResponse,
  mockScrollToAndApplyTop,
  renderApp,
  renderAppWithProjectAndSession,
  settleAsyncUi,
  stubElementScrollGeometry,
  withVerifiedNoReactActWarnings,
} from "./app-test-harness";

import {
  installAppScrollTestHarness,
  scrollToTopsWithBehavior,
} from "./App.scroll-behavior.fixtures";

describe("App prompt-send and live-turn scroll", () => {
  installAppScrollTestHarness();

  it("reattaches on Send but preserves a later user scroll while the send is in flight", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: 1000,
      });
      const scrollToMock = mockScrollToAndApplyTop();
      const context = await renderAppWithProjectAndSession();
      const fixtureFetch = context.fetchMock.getMockImplementation();
      if (!fixtureFetch) {
        throw new Error("Expected the shared fetch fixture implementation");
      }
      const pendingSend = createDeferred<Response>();
      const baseState = {
        revision: 2,
        projects: [
          {
            id: "project-termal",
            name: "TermAl",
            rootPath: "/projects/termal",
          },
        ],
        sessions: [
          makeSession("session-1", {
            name: "Session 1",
            projectId: "project-termal",
            workdir: "/projects/termal",
            preview: "Latest user prompt.",
            messages: [
              {
                id: "message-user-1",
                type: "text",
                timestamp: "10:01",
                author: "you",
                text: "Latest user prompt.",
              },
            ],
          }),
        ],
      };

      context.fetchMock.mockImplementation(async (input: RequestInfo | URL) => {
        const requestUrl = new URL(String(input), "http://localhost");
        if (requestUrl.pathname === "/api/state") {
          return jsonResponse(baseState);
        }
        if (requestUrl.pathname === "/api/sessions/session-1/messages") {
          return pendingSend.promise;
        }
        return fixtureFetch(input);
      });

      try {
        await dispatchStateEvent(latestEventSource(), baseState);
        await settleAsyncUi();

        const messageStack = document.querySelector(
          ".workspace-pane.active .message-stack",
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }

        act(() => {
          fireEvent.wheel(messageStack, { deltaY: -800 });
          messageStack.scrollTop = 0;
          fireEvent.scroll(messageStack);
        });

        const composer = await screen.findByLabelText("Message Session 1");
        if (!(composer instanceof HTMLTextAreaElement)) {
          throw new Error("Composer textarea not found");
        }

        await act(async () => {
          fireEvent.change(composer, {
            target: { value: "Follow this prompt" },
          });
        });

        scrollToMock.mockClear();

        await act(async () => {
          fireEvent.click(screen.getByRole("button", { name: "Send" }));
          await Promise.resolve();
        });
        await settleAsyncUi();

        expect(messageStack.scrollTop).toBe(800);
        expect(messageStack).toHaveClass("is-tail-following");
        expect(
          screen.queryByRole("button", { name: "New activity" }),
        ).not.toBeInTheDocument();

        act(() => {
          fireEvent.wheel(messageStack, { deltaY: -400 });
          fireEvent.scroll(messageStack);
        });
        expect(messageStack.scrollTop).toBe(400);
        expect(messageStack).not.toHaveClass("is-tail-following");
        scrollToMock.mockClear();
        await settleAsyncUi();
        expect(messageStack.scrollTop).toBe(400);
        expect(scrollToTopsWithBehavior(scrollToMock, "auto")).toHaveLength(0);

        context.cleanup();
        await flushUiWork();
        expect(scrollToTopsWithBehavior(scrollToMock, "auto")).toHaveLength(0);
      } finally {
        restoreScrollGeometry();
      }
    });
  });

  it.each([false, true])(
    "loads the true tail on Send and respects intervening reader input (%s)",
    async (navigateBeforeTailArrives) => {
      await withVerifiedNoReactActWarnings(async () => {
        const restoreScrollGeometry = stubElementScrollGeometry({
          clientHeight: 200,
          scrollHeight: 1000,
        });
        mockScrollToAndApplyTop();
        const pendingTail = createDeferred<
          Awaited<ReturnType<typeof api.fetchSessionHistory>>
        >();
        const historicalMessage = {
          id: "historical-message",
          type: "text" as const,
          author: "assistant" as const,
          timestamp: "10:00",
          text: "Older response",
        };
        const fetchHistorySpy = vi
          .spyOn(api, "fetchSessionHistory")
          .mockImplementation((_sessionId, options) =>
            options.around !== undefined
              ? Promise.resolve({
                  messages: [historicalMessage],
                  nextBefore: historicalMessage.id,
                  hasMore: true,
                  nextAfter: historicalMessage.id,
                  hasNewer: true,
                  messageStartIndex: 500,
                  messageCount: 1_000,
                  revision: 2,
                  sessionMutationStamp: 2,
                  serverInstanceId: "test-instance",
                })
              : pendingTail.promise,
          );
        const pendingSend = createDeferred<
          Awaited<ReturnType<typeof api.sendMessage>>
        >();
        vi.spyOn(api, "sendMessage").mockImplementation(
          () => pendingSend.promise,
        );
        const context = await renderAppWithProjectAndSession();
        try {
          await act(async () => {
            expect(
              await requestSessionHistoryAroundPage("session-1", 500),
            ).toBe(true);
            await flushUiWork();
          });
          await settleAsyncUi();
          fetchHistorySpy.mockClear();
          const messageStack = document.querySelector(
            ".workspace-pane.active .message-stack",
          );
          if (!(messageStack instanceof HTMLElement)) {
            throw new Error("Message stack not found");
          }
          messageStack.scrollTop = 400;
          expect(messageStack).not.toHaveClass("is-tail-following");
          expect(fetchHistorySpy).not.toHaveBeenCalled();
          await act(async () => {
            fireEvent.change(screen.getByLabelText("Message Session 1"), {
              target: { value: "Return to live output" },
            });
          });
          await clickAndSettle(screen.getByRole("button", { name: "Send" }));
          expect(fetchHistorySpy).toHaveBeenCalledTimes(1);
          expect(fetchHistorySpy.mock.calls[0]?.[1].from).toBeUndefined();
          expect(fetchHistorySpy.mock.calls[0]?.[1].before).toBeUndefined();
          expect(fetchHistorySpy.mock.calls[0]?.[1].after).toBeUndefined();
          expect(messageStack.scrollTop).toBe(400);

          expect(
            getSessionRecordSnapshotForTesting("session-1")?.hasNewerHistory,
          ).toBe(true);

          if (navigateBeforeTailArrives) {
            act(() => {
              fireEvent.keyDown(messageStack, {
                key: "ArrowUp",
                code: "ArrowUp",
              });
              fireEvent.scroll(messageStack);
            });
            expect(messageStack.scrollTop).toBe(360);
          }
          await act(async () => {
            pendingTail.resolve({
              messages: [
                {
                  ...historicalMessage,
                  id: "tail-message",
                  text: "Latest response",
                },
              ],
              nextBefore: "tail-message",
              hasMore: true,
              nextAfter: null,
              hasNewer: false,
              messageStartIndex: 999,
              messageCount: 1_000,
              revision: 3,
              sessionMutationStamp: 3,
              serverInstanceId: "test-instance",
            });
            await flushUiWork();
          });
          await settleAsyncUi();
          expect(
            getSessionRecordSnapshotForTesting("session-1")?.hasNewerHistory,
          ).toBe(false);
          expect(messageStack.scrollTop).toBe(
            navigateBeforeTailArrives ? 360 : 800,
          );
          expect(messageStack.classList.contains("is-tail-following")).toBe(
            !navigateBeforeTailArrives,
          );
        } finally {
          context.cleanup();
          restoreScrollGeometry();
        }
      });
    },
  );

  it.each([
    ["before the first follow frame", 0],
    ["after several follow frames", 3],
  ])(
    "keeps a growing near-bottom send inside one live follow %s",
    async (_timing, framesBeforeGrowth) => {
    await withVerifiedNoReactActWarnings(async () => {
      let scrollHeight = 1000;
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: () => scrollHeight,
      });
      const scrollToMock = mockScrollToAndApplyTop();
      const context = await renderAppWithProjectAndSession();
      const pendingSend = createDeferred<Response>();
      const baseState = {
        revision: 2,
        projects: [
          {
            id: "project-termal",
            name: "TermAl",
            rootPath: "/projects/termal",
          },
        ],
        sessions: [
          makeSession("session-1", {
            name: "Session 1",
            projectId: "project-termal",
            workdir: "/projects/termal",
            preview: "Latest user prompt.",
            messages: [
              {
                id: "message-user-1",
                type: "text",
                timestamp: "10:01",
                author: "you",
                text: "Latest user prompt.",
              },
            ],
          }),
        ],
      };

      context.fetchMock.mockImplementation(async (input: RequestInfo | URL) => {
        const requestUrl = new URL(String(input), "http://localhost");
        if (requestUrl.pathname === "/api/state") {
          return jsonResponse(baseState);
        }
        if (requestUrl.pathname === "/api/sessions/session-1/messages") {
          return pendingSend.promise;
        }
        throw new Error(`Unexpected fetch: ${requestUrl.pathname}`);
      });

      try {
        await dispatchStateEvent(latestEventSource(), baseState);
        await settleAsyncUi();

        const messageStack = document.querySelector(
          ".workspace-pane.active .message-stack",
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }

        // Establish an attached near-bottom position through a browser clamp,
        // not through an unannounced upward frame (which is reader movement).
        scrollHeight = 960;
        messageStack.scrollTop = 760;
        act(() => {
          fireEvent.scroll(messageStack);
        });
        expect(messageStack).toHaveClass("is-tail-following");

        const composer = await screen.findByLabelText("Message Session 1");
        if (!(composer instanceof HTMLTextAreaElement)) {
          throw new Error("Composer textarea not found");
        }

        await act(async () => {
          fireEvent.change(composer, {
            target: { value: "Near-bottom prompt" },
          });
        });

        scrollToMock.mockClear();

        const originalRequestAnimationFrame = window.requestAnimationFrame;
        const originalCancelAnimationFrame = window.cancelAnimationFrame;
        const animationFrames = new Map<number, FrameRequestCallback>();
        let nextFrameId = 1;
        window.requestAnimationFrame = ((callback: FrameRequestCallback) => {
          const frameId = nextFrameId;
          nextFrameId += 1;
          animationFrames.set(frameId, callback);
          return frameId;
        }) as typeof requestAnimationFrame;
        window.cancelAnimationFrame = ((frameId: number) => {
          animationFrames.delete(frameId);
        }) as typeof cancelAnimationFrame;
        try {
          await act(async () => {
            fireEvent.click(screen.getByRole("button", { name: "Send" }));
            await Promise.resolve();
          });
          let frameTimestamp = 0;
          for (let index = 0; index < framesBeforeGrowth; index += 1) {
            const nextFrame = animationFrames.entries().next().value;
            if (!nextFrame) {
              throw new Error("Live follow ended before delayed geometry");
            }
            animationFrames.delete(nextFrame[0]);
            frameTimestamp += 1000 / 60;
            act(() => nextFrame[1](frameTimestamp));
          }
          scrollHeight = 1120;
          let drainedFrames = 0;
          while (animationFrames.size > 0 && drainedFrames < 80) {
            const nextFrame = animationFrames.entries().next().value;
            if (!nextFrame) {
              break;
            }
            animationFrames.delete(nextFrame[0]);
            frameTimestamp += 1000 / 60;
            act(() => nextFrame[1](frameTimestamp));
            drainedFrames += 1;
          }
        } finally {
          window.requestAnimationFrame = originalRequestAnimationFrame;
          window.cancelAnimationFrame = originalCancelAnimationFrame;
        }
        await settleAsyncUi();

        const followedTops = scrollToTopsWithBehavior(scrollToMock, "auto");
        expect(followedTops.length).toBeGreaterThan(0);
        expect(Math.max(...followedTops)).toBeGreaterThan(760);
      } finally {
        context.cleanup();
        restoreScrollGeometry();
      }
      });
    },
  );

  it("makes room when the first agent card appears above the live turn", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      let scrollHeight = 1000;
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: () => scrollHeight,
      });
      const scrollToMock = mockScrollToAndApplyTop();
      const context = await renderAppWithProjectAndSession();
      const messages: Session["messages"] = [
        {
          id: "message-user-1",
          type: "text",
          timestamp: "10:00",
          author: "you",
          text: "Current prompt",
        },
        {
          id: "message-assistant-1",
          type: "text",
          timestamp: "10:01",
          author: "assistant",
          text: "Current response.",
        },
      ];
      const baseState = {
        revision: 2,
        projects: [
          {
            id: "project-termal",
            name: "TermAl",
            rootPath: "/projects/termal",
          },
        ],
        sessions: [
          makeSession("session-1", {
            name: "Session 1",
            projectId: "project-termal",
            workdir: "/projects/termal",
            preview: "Current response.",
            messages,
          }),
        ],
      };

      try {
        await dispatchStateEvent(latestEventSource(), baseState);
        await settleAsyncUi();

        const messageStack = Array.from(
          document.querySelectorAll(".message-stack"),
        ).find(
          (candidate): candidate is HTMLElement =>
            candidate instanceof HTMLElement &&
            !candidate.classList.contains("control-panel-stack"),
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }

        messageStack.scrollTop = 800;
        await act(async () => {
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });
        scrollToMock.mockClear();

        scrollHeight = 1120;
        await dispatchStateEvent(latestEventSource(), {
          ...baseState,
          revision: 3,
          sessions: [
            makeSession("session-1", {
              name: "Session 1",
              projectId: "project-termal",
              workdir: "/projects/termal",
              status: "active",
              preview: "Current response.",
              messages,
            }),
          ],
        });
        await settleAsyncUi();

        expect(screen.getByText("Live turn")).toBeInTheDocument();
        expect(messageStack).toHaveClass("is-tail-following");
        expect(
          Math.max(...scrollToTopsWithBehavior(scrollToMock, "auto")),
        ).toBeGreaterThan(800);
        expect(messageStack.scrollTop).toBeGreaterThan(800);
        expect(messageStack.scrollTop).toBeLessThanOrEqual(920);
      } finally {
        context.cleanup();
        restoreScrollGeometry();
      }
    });
  });

  it("follows live-turn growth while its visible pane is unfocused", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      let scrollHeight = 1000;
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: () => scrollHeight,
      });
      const scrollToMock = mockScrollToAndApplyTop();
      const messages: Session["messages"] = [
        {
          id: "message-user-1",
          type: "text",
          timestamp: "10:00",
          author: "you",
          text: "Current prompt",
        },
        {
          id: "message-assistant-1",
          type: "text",
          timestamp: "10:01",
          author: "assistant",
          text: "Current response.",
        },
      ];
      const fetchMock = vi.fn(async (input: RequestInfo | URL) => {
        const requestUrl = new URL(String(input), "http://localhost");
        if (requestUrl.pathname === "/api/state") {
          return jsonResponse(
            makeStateResponse({
              revision: 1,
              projects: [
                {
                  id: "project-termal",
                  name: "TermAl",
                  rootPath: "/projects/termal",
                },
              ],
              orchestrators: [],
              workspaces: [],
              sessions: [
                makeSession("session-1", {
                  name: "Session 1",
                  projectId: "project-termal",
                  workdir: "/projects/termal",
                  preview: "Current response.",
                  messages,
                }),
              ],
            }),
          );
        }
        throw new Error(`Unexpected fetch: ${requestUrl.pathname}`);
      });

      const layoutStorageKey = `${WORKSPACE_LAYOUT_STORAGE_KEY}:test-waiting-indicator-inactive-edge`;
      window.history.replaceState(
        window.history.state,
        "",
        "/?workspace=test-waiting-indicator-inactive-edge",
      );
      window.localStorage.clear();
      window.localStorage.setItem(
        layoutStorageKey,
        JSON.stringify({
          controlPanelSide: "left",
          workspace: {
            root: {
              id: "split-root",
              type: "split",
              direction: "row",
              ratio: 0.35,
              first: {
                type: "pane",
                paneId: "pane-control",
              },
              second: {
                type: "pane",
                paneId: "pane-session",
              },
            },
            panes: [
              {
                id: "pane-control",
                tabs: [
                  {
                    id: "tab-control",
                    kind: "controlPanel",
                    originSessionId: null,
                  },
                ],
                activeTabId: "tab-control",
                activeSessionId: null,
                viewMode: "controlPanel",
                lastSessionViewMode: "session",
                sourcePath: null,
              },
              {
                id: "pane-session",
                tabs: [
                  {
                    id: "tab-session",
                    kind: "session",
                    sessionId: "session-1",
                  },
                ],
                activeTabId: "tab-session",
                activeSessionId: "session-1",
                viewMode: "session",
                lastSessionViewMode: "session",
                sourcePath: null,
              },
            ],
            activePaneId: "pane-control",
          },
        }),
      );

      vi.stubGlobal("fetch", fetchMock);
      vi.stubGlobal(
        "EventSource",
        EventSourceMock as unknown as typeof EventSource,
      );
      vi.stubGlobal(
        "ResizeObserver",
        ResizeObserverMock as unknown as typeof ResizeObserver,
      );

      try {
        await renderApp();
        act(() => {
          latestEventSource().dispatchError();
        });
        await settleAsyncUi();

        const messageStack = Array.from(
          document.querySelectorAll(".message-stack"),
        ).find(
          (candidate): candidate is HTMLElement =>
            candidate instanceof HTMLElement &&
            !candidate.classList.contains("control-panel-stack"),
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Session message stack not found");
        }
        const sessionPane = messageStack.closest(".workspace-pane");
        if (!(sessionPane instanceof HTMLElement)) {
          throw new Error("Session pane not found");
        }
        expect(sessionPane).not.toHaveClass("active");

        messageStack.scrollTop = 800;
        await act(async () => {
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });
        scrollToMock.mockClear();

        scrollHeight = 1120;
        await dispatchStateEvent(latestEventSource(), {
          revision: 2,
          projects: [
            {
              id: "project-termal",
              name: "TermAl",
              rootPath: "/projects/termal",
            },
          ],
          sessions: [
            makeSession("session-1", {
              name: "Session 1",
              projectId: "project-termal",
              workdir: "/projects/termal",
              status: "active",
              preview: "Current response.",
              messages,
            }),
          ],
        });
        await settleAsyncUi();

        expect(screen.getByText("Live turn")).toBeInTheDocument();
        expect(messageStack.scrollTop).toBe(920);
        expect(filterScrollToCallsAt(scrollToMock, 920, "auto")).toHaveLength(1);
        scrollToMock.mockClear();

        await act(async () => {
          fireEvent.mouseDown(sessionPane);
          await flushUiWork();
        });
        await settleAsyncUi();

        expect(sessionPane).toHaveClass("active");
        expect(messageStack.scrollTop).toBe(920);
        expect(filterScrollToCallsAt(scrollToMock, 920, "auto")).toEqual([]);
      } finally {
        restoreScrollGeometry();
      }
    });
  });

  it("does not repeat bottom-follow while the live waiting indicator remains visible", async () => {
    await withVerifiedNoReactActWarnings(async () => {
      let scrollHeight = 1000;
      const restoreScrollGeometry = stubElementScrollGeometry({
        clientHeight: 200,
        scrollHeight: () => scrollHeight,
      });
      const scrollToMock = mockScrollToAndApplyTop();
      const context = await renderAppWithProjectAndSession();
      const messages: Session["messages"] = [
        {
          id: "message-user-1",
          type: "text",
          timestamp: "10:00",
          author: "you",
          text: "Current prompt",
        },
        {
          id: "message-assistant-1",
          type: "text",
          timestamp: "10:01",
          author: "assistant",
          text: "Current response.",
        },
      ];
      const baseState = {
        revision: 2,
        projects: [
          {
            id: "project-termal",
            name: "TermAl",
            rootPath: "/projects/termal",
          },
        ],
        sessions: [
          makeSession("session-1", {
            name: "Session 1",
            projectId: "project-termal",
            workdir: "/projects/termal",
            preview: "Current response.",
            messages,
          }),
        ],
      };

      try {
        await dispatchStateEvent(latestEventSource(), baseState);
        await settleAsyncUi();

        const messageStack = Array.from(
          document.querySelectorAll(".message-stack"),
        ).find(
          (candidate): candidate is HTMLElement =>
            candidate instanceof HTMLElement &&
            !candidate.classList.contains("control-panel-stack"),
        );
        if (!(messageStack instanceof HTMLElement)) {
          throw new Error("Message stack not found");
        }

        messageStack.scrollTop = 800;
        await act(async () => {
          fireEvent.scroll(messageStack);
          await flushUiWork();
        });
        scrollToMock.mockClear();

        scrollHeight = 1120;
        await dispatchStateEvent(latestEventSource(), {
          ...baseState,
          revision: 3,
          sessions: [
            makeSession("session-1", {
              name: "Session 1",
              projectId: "project-termal",
              workdir: "/projects/termal",
              status: "active",
              preview: "Current response.",
              messages,
            }),
          ],
        });
        await settleAsyncUi();

        expect(screen.getByText("Live turn")).toBeInTheDocument();
        expect(
          Math.max(...scrollToTopsWithBehavior(scrollToMock, "auto")),
        ).toBeGreaterThan(800);
        expect(messageStack.scrollTop).toBeGreaterThan(800);
        expect(messageStack.scrollTop).toBeLessThanOrEqual(920);

        scrollToMock.mockClear();
        scrollHeight = 1140;
        await dispatchStateEvent(latestEventSource(), {
          ...baseState,
          revision: 4,
          sessions: [
            makeSession("session-1", {
              name: "Session 1",
              projectId: "project-termal",
              workdir: "/projects/termal",
              status: "active",
              preview: "Current response.",
              messages,
            }),
          ],
        });
        await settleAsyncUi();

        expect(screen.getByText("Live turn")).toBeInTheDocument();
        expect(filterScrollToCallsAt(scrollToMock, 940, "auto")).toEqual([]);
        expect(messageStack.scrollTop).toBeGreaterThan(800);
        expect(messageStack.scrollTop).toBeLessThanOrEqual(920);
      } finally {
        context.cleanup();
        restoreScrollGeometry();
      }
    });
  });

});
