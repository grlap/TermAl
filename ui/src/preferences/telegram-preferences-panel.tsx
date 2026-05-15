// Owns the Telegram settings UI: bot-token entry, status loading, project/session
// defaults, and relay enable/save/test controls.
// Does not own Telegram transport, backend relay lifecycle, or other
// preference panels.
// Split from ui/src/preferences-panels.tsx.
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import {
  fetchTelegramStatus,
  testTelegramConnection,
  updateTelegramConfig,
  type TelegramStatusResponse,
} from "../api";
import type { Project, Session } from "../types";
import type { ComboboxOption } from "../session-model-utils";
import { ThemedCombobox } from "./themed-combobox";

type TelegramSettingsDraft = {
  enabled: boolean;
  botToken: string;
  subscribedProjectIds: string[];
  defaultProjectId: string;
  defaultSessionId: string;
};

function createTelegramDraft(
  status: TelegramStatusResponse | null,
): TelegramSettingsDraft {
  return {
    enabled: status?.enabled ?? false,
    botToken: "",
    subscribedProjectIds: status?.subscribedProjectIds ?? [],
    defaultProjectId: status?.defaultProjectId ?? "",
    defaultSessionId: status?.defaultSessionId ?? "",
  };
}

function telegramStatusLabel(status: TelegramStatusResponse | null): string {
  if (!status) {
    return "Loading";
  }
  if (status.running) {
    return "Polling";
  }
  if (status.lifecycle === "inProcess" && status.enabled && status.configured) {
    return "Stopped";
  }
  if (status.linkedChatId !== null && status.linkedChatId !== undefined) {
    return "Linked";
  }
  if (status.configured) {
    return "Configured";
  }
  return "Not configured";
}

export function TelegramPreferencesPanel({
  projects,
  sessions,
}: {
  projects: Project[];
  sessions: Session[];
}) {
  const [status, setStatus] = useState<TelegramStatusResponse | null>(null);
  const [draft, setDraft] = useState<TelegramSettingsDraft>(() =>
    createTelegramDraft(null),
  );
  const [isLoading, setIsLoading] = useState(true);
  const [isSaving, setIsSaving] = useState(false);
  const [isTesting, setIsTesting] = useState(false);
  const [isSetupOpen, setIsSetupOpen] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const isMountedRef = useRef(true);
  const statusFetchVersionRef = useRef(0);

  useEffect(() => {
    isMountedRef.current = true;
    return () => {
      isMountedRef.current = false;
    };
  }, []);

  useEffect(() => {
    const fetchVersion = statusFetchVersionRef.current + 1;
    statusFetchVersionRef.current = fetchVersion;
    const isCurrentStatusFetch = () =>
      isMountedRef.current && statusFetchVersionRef.current === fetchVersion;

    setIsLoading(true);
    fetchTelegramStatus()
      .then((nextStatus) => {
        if (!isCurrentStatusFetch()) {
          return;
        }
        setStatus(nextStatus);
        setDraft(createTelegramDraft(nextStatus));
        setError(null);
      })
      .catch((loadError: unknown) => {
        if (isCurrentStatusFetch()) {
          setError(
            loadError instanceof Error
              ? loadError.message
              : "Failed to load Telegram settings.",
          );
        }
      })
      .finally(() => {
        if (isCurrentStatusFetch()) {
          setIsLoading(false);
        }
      });

    return () => {
      if (statusFetchVersionRef.current === fetchVersion) {
        statusFetchVersionRef.current += 1;
      }
    };
  }, []);

  const projectOptions = useMemo<ComboboxOption[]>(
    () => [
      { label: "No default project", value: "" },
      ...projects.map((project) => ({
        label: project.name,
        value: project.id,
        description: project.rootPath,
      })),
    ],
    [projects],
  );
  const defaultProjectSessions = useMemo(
    () =>
      sessions.filter(
        (session) =>
          draft.defaultProjectId &&
          session.projectId === draft.defaultProjectId,
      ),
    [draft.defaultProjectId, sessions],
  );
  const sessionOptions = useMemo<ComboboxOption[]>(
    () => [
      { label: "No default session", value: "" },
      ...defaultProjectSessions.map((session) => ({
        label: session.name,
        value: session.id,
        description: `${session.agent} - ${session.status}`,
      })),
    ],
    [defaultProjectSessions],
  );
  const subscribedProjectIds = useMemo(
    () => new Set(draft.subscribedProjectIds),
    [draft.subscribedProjectIds],
  );
  const selectedDefaultSessionExists =
    draft.defaultSessionId === "" ||
    defaultProjectSessions.some(
      (session) => session.id === draft.defaultSessionId,
    );
  const canTestToken =
    draft.botToken.trim().length > 0 || Boolean(status?.botTokenMasked);
  const hasSavedToken = Boolean(status?.botTokenMasked);

  const updateDraft = useCallback((patch: Partial<TelegramSettingsDraft>) => {
    setDraft((current) => ({ ...current, ...patch }));
    setNotice(null);
    setError(null);
  }, []);

  const toggleProject = useCallback((projectId: string, checked: boolean) => {
    setDraft((current) => {
      const nextIds = checked
        ? [...current.subscribedProjectIds, projectId]
        : current.subscribedProjectIds.filter(
            (candidate) => candidate !== projectId,
          );
      const removesDefaultProject =
        !checked && current.defaultProjectId === projectId;
      return {
        ...current,
        subscribedProjectIds: Array.from(new Set(nextIds)),
        defaultProjectId: removesDefaultProject ? "" : current.defaultProjectId,
        defaultSessionId: removesDefaultProject ? "" : current.defaultSessionId,
      };
    });
    setNotice(null);
    setError(null);
  }, []);

  const handleDefaultProjectChange = useCallback(
    (nextProjectId: string) => {
      updateDraft({
        defaultProjectId: nextProjectId,
        defaultSessionId: "",
        subscribedProjectIds: nextProjectId
          ? Array.from(
              new Set([...draft.subscribedProjectIds, nextProjectId]),
            )
          : draft.subscribedProjectIds,
      });
    },
    [draft.subscribedProjectIds, updateDraft],
  );

  const handleDefaultSessionChange = useCallback(
    (nextSessionId: string) => {
      updateDraft({ defaultSessionId: nextSessionId });
    },
    [updateDraft],
  );

  const handleSave = useCallback(async () => {
    const nextProjectIds = draft.defaultProjectId
      ? Array.from(
          new Set([...draft.subscribedProjectIds, draft.defaultProjectId]),
        )
      : draft.subscribedProjectIds;
    if (draft.enabled && canTestToken && nextProjectIds.length === 0) {
      setNotice(null);
      setError("Choose at least one Telegram project before enabling the relay.");
      return;
    }

    setIsSaving(true);
    setNotice(null);
    setError(null);
    try {
      const nextStatus = await updateTelegramConfig({
        enabled: draft.enabled,
        botToken: draft.botToken.trim() ? draft.botToken.trim() : undefined,
        subscribedProjectIds: nextProjectIds,
        defaultProjectId: draft.defaultProjectId || null,
        defaultSessionId:
          draft.defaultSessionId && selectedDefaultSessionExists
            ? draft.defaultSessionId
            : null,
      });
      if (!isMountedRef.current) {
        return;
      }
      setStatus(nextStatus);
      setDraft(createTelegramDraft(nextStatus));
      setNotice("Telegram settings saved.");
    } catch (saveError: unknown) {
      if (isMountedRef.current) {
        setError(
          saveError instanceof Error
            ? saveError.message
            : "Failed to save Telegram settings.",
        );
      }
    } finally {
      if (isMountedRef.current) {
        setIsSaving(false);
      }
    }
  }, [canTestToken, draft, selectedDefaultSessionExists]);

  const handleTestConnection = useCallback(async () => {
    setIsTesting(true);
    setNotice(null);
    setError(null);
    try {
      const trimmedToken = draft.botToken.trim();
      const result = await testTelegramConnection(
        trimmedToken ? { botToken: trimmedToken } : { useSavedToken: true },
      );
      if (!isMountedRef.current) {
        return;
      }
      setNotice(
        result.botUsername
          ? `Connected to @${result.botUsername}.`
          : `Connected to ${result.botName}.`,
      );
    } catch (testError: unknown) {
      if (isMountedRef.current) {
        setError(
          testError instanceof Error
            ? testError.message
            : "Telegram connection test failed.",
        );
      }
    } finally {
      if (isMountedRef.current) {
        setIsTesting(false);
      }
    }
  }, [draft.botToken]);

  const handleRemoveToken = useCallback(async () => {
    setIsSaving(true);
    setNotice(null);
    setError(null);
    try {
      const nextStatus = await updateTelegramConfig({
        enabled: false,
        botToken: null,
      });
      if (!isMountedRef.current) {
        return;
      }
      setStatus(nextStatus);
      setDraft(createTelegramDraft(nextStatus));
      setNotice("Telegram bot token removed.");
    } catch (removeError: unknown) {
      if (isMountedRef.current) {
        setError(
          removeError instanceof Error
            ? removeError.message
            : "Failed to remove Telegram bot token.",
        );
      }
    } finally {
      if (isMountedRef.current) {
        setIsSaving(false);
      }
    }
  }, []);

  return (
    <section className="settings-panel-stack">
      <article className="message-card prompt-settings-card telegram-settings-card">
        <div className="telegram-settings-header">
          <div>
            <div className="card-label">Mobile</div>
            <h3>Telegram</h3>
          </div>
          <span className="remote-settings-badge">
            {telegramStatusLabel(status)}
          </span>
        </div>

        <div className="prompt-settings-grid telegram-settings-grid">
          <div className="session-control-group telegram-token-group">
            <label className="session-control-label" htmlFor="telegram-bot-token">
              Bot token
            </label>
            <input
              id="telegram-bot-token"
              className="themed-input"
              type="password"
              value={draft.botToken}
              placeholder={status?.botTokenMasked ?? "BotFather token"}
              disabled={isLoading || isSaving}
              autoComplete="off"
              onChange={(event) => updateDraft({ botToken: event.target.value })}
            />
            {hasSavedToken ? (
              <p className="session-control-hint">
                Stored in the OS credential store as {status?.botTokenMasked}.
              </p>
            ) : null}
          </div>

          <label className="remote-settings-toggle telegram-enable-toggle">
            <input
              type="checkbox"
              checked={draft.enabled}
              disabled={isLoading || isSaving}
              onChange={(event) => updateDraft({ enabled: event.target.checked })}
            />
            <span>Enable relay</span>
          </label>

          <div className="session-control-group">
            <label
              className="session-control-label"
              htmlFor="telegram-default-project"
            >
              Default project
            </label>
            <ThemedCombobox
              id="telegram-default-project"
              value={draft.defaultProjectId}
              options={projectOptions}
              disabled={isLoading || isSaving || projects.length === 0}
              onChange={handleDefaultProjectChange}
            />
          </div>

          <div className="session-control-group">
            <label
              className="session-control-label"
              htmlFor="telegram-default-session"
            >
              Default session
            </label>
            <ThemedCombobox
              id="telegram-default-session"
              value={selectedDefaultSessionExists ? draft.defaultSessionId : ""}
              options={sessionOptions}
              disabled={
                isLoading ||
                isSaving ||
                !draft.defaultProjectId ||
                defaultProjectSessions.length === 0
              }
              onChange={handleDefaultSessionChange}
            />
          </div>
        </div>

        <div
          className="telegram-project-list"
          aria-label="Telegram subscribed projects"
        >
          {projects.length > 0 ? (
            projects.map((project) => (
              <label key={project.id} className="telegram-project-option">
                <input
                  type="checkbox"
                  checked={subscribedProjectIds.has(project.id)}
                  disabled={isLoading || isSaving}
                  onChange={(event) =>
                    toggleProject(project.id, event.target.checked)
                  }
                />
                <span>
                  <strong>{project.name}</strong>
                  <span>{project.rootPath}</span>
                </span>
              </label>
            ))
          ) : (
            <p className="session-control-hint">No projects yet.</p>
          )}
        </div>

        <div className="remote-settings-actions telegram-settings-actions">
          <button
            className="ghost-button"
            type="button"
            aria-expanded={isSetupOpen}
            aria-controls="telegram-setup-panel"
            onClick={() => setIsSetupOpen((current) => !current)}
          >
            {isSetupOpen ? "Hide setup" : "Setup"}
          </button>
          <button
            className="ghost-button"
            type="button"
            onClick={handleTestConnection}
            disabled={isLoading || isTesting || !canTestToken}
          >
            {isTesting ? "Testing..." : "Test connection"}
          </button>
          <button
            className="ghost-button"
            type="button"
            onClick={handleRemoveToken}
            disabled={isLoading || isSaving || !hasSavedToken}
          >
            Remove token
          </button>
          <button
            className="ghost-button"
            type="button"
            disabled
            title="Manual chat linking is active; the link-code wizard lands in a later Telegram phase."
          >
            Link chat
          </button>
          <button
            className="send-button"
            type="button"
            onClick={handleSave}
            disabled={isLoading || isSaving}
          >
            {isSaving ? "Saving..." : "Save Telegram"}
          </button>
        </div>

        {isSetupOpen ? (
          <div id="telegram-setup-panel" className="telegram-setup-panel">
            <p className="session-control-label">Setup flow</p>
            <ol>
              <li>
                Open @BotFather in Telegram, create a bot with /newbot, and
                copy the token.
              </li>
              <li>
                Paste the token here, test the connection, choose projects,
                then save.
              </li>
              <li>
                Turn on Enable relay. TermAl will run the Telegram relay from
                the main backend process.
              </li>
              <li>Open the bot in Telegram and send /start from the linked chat.</li>
            </ol>
            <p className="session-control-hint">
              No separate TermAl process or telegram command is required. Use
              /projects and /project &lt;id&gt; in Telegram to switch projects,
              then /sessions and /session &lt;id&gt; to switch sessions.
            </p>
          </div>
        ) : null}

        {status?.linkedChatId !== null && status?.linkedChatId !== undefined ? (
          <p className="session-control-hint">
            Linked chat id: {status.linkedChatId}
          </p>
        ) : null}
        {status?.enabled && status.lifecycle === "manual" ? (
          <p className="session-control-hint">
            Relay startup from this toggle lands in the next backend lifecycle
            phase.
          </p>
        ) : null}
        {notice ? (
          <p className="session-control-hint telegram-settings-notice">
            {notice}
          </p>
        ) : null}
        {error ? (
          <p className="session-control-hint telegram-settings-error">
            {error}
          </p>
        ) : null}
      </article>
    </section>
  );
}
