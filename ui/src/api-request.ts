// Owns low-level API request helpers, including JSON-first state fetch parsing
// and HTML fallback restart classification. Does not own typed route wrappers.
// Split from: ui/src/api.ts.
import { sanitizeUserFacingErrorMessage } from "./error-messages";

const JSON_HTML_FALLBACK_TEXT_LIMIT_BYTES = 64 * 1024;

/**
 * High-level request failure category for UI recovery paths.
 *
 * Do not treat `"request-failed"` as a non-5xx guarantee. Routes that opt into
 * `preserveGatewayErrorBody` intentionally keep parseable 502/503/504 JSON
 * error bodies as `"request-failed"` so callers can surface actionable
 * upstream diagnostics.
 * Branch on `status` and `restartRequired` when status-class behavior matters.
 */
export type ApiRequestErrorKind = "backend-unavailable" | "request-failed";

export class ApiRequestError extends Error {
  declare readonly cause: unknown;
  readonly kind: ApiRequestErrorKind;
  readonly status: number | null;
  readonly restartRequired: boolean;

  constructor(
    kind: ApiRequestErrorKind,
    message: string,
    options?: {
      status?: number | null;
      restartRequired?: boolean;
      cause?: unknown;
    },
  ) {
    // TypeScript's current lib target in this repo does not yet model the ES2022
    // Error options bag, but supported runtimes do and tooling reads it there.
    // @ts-expect-error ES2022 Error options are available at runtime.
    super(message, { cause: options?.cause });
    this.name = "ApiRequestError";
    this.kind = kind;
    this.status = options?.status ?? null;
    this.restartRequired = options?.restartRequired ?? false;
    Object.setPrototypeOf(this, new.target.prototype);
  }
}

export function isBackendUnavailableError(
  error: unknown,
): error is ApiRequestError {
  return (
    error instanceof ApiRequestError && error.kind === "backend-unavailable"
  );
}

export async function request<T>(
  path: string,
  init?: RequestInit,
  options?: {
    /**
     * Keep parseable 502/503/504 JSON error bodies as request-failed details
     * instead of mapping them to the generic backend-unavailable UI path. Use
     * this only for routes that deliberately proxy a third-party service and
     * return actionable JSON errors for upstream failures.
     */
    preserveGatewayErrorBody?: boolean;
  },
): Promise<T> {
  const response = await performRequest(path, init);

  const contentType = response.headers.get("content-type") ?? "";
  const raw = await response.text();
  if (looksLikeHtmlResponse(raw, contentType)) {
    throw createBackendUnavailableError(
      formatUnavailableApiMessage(path, response.status),
      response.status,
      { restartRequired: true },
    );
  }

  if (!response.ok) {
    throw createResponseError(raw, response.status, options);
  }

  if (!raw) {
    return {} as T;
  }

  return JSON.parse(raw) as T;
}

export async function requestJsonFirst<T>(
  path: string,
  init?: RequestInit,
  options?: {
    preserveGatewayErrorBody?: boolean;
  },
): Promise<T> {
  const response = await performRequest(path, init);
  const contentType = response.headers.get("content-type") ?? "";

  if (looksLikeHtmlResponse("", contentType)) {
    await response.text().catch(() => "");
    throw createBackendUnavailableError(
      formatUnavailableApiMessage(path, response.status),
      response.status,
      { restartRequired: true },
    );
  }

  if (!response.ok) {
    const raw = await response.text();
    if (looksLikeHtmlResponse(raw, contentType)) {
      throw createBackendUnavailableError(
        formatUnavailableApiMessage(path, response.status),
        response.status,
        { restartRequired: true },
      );
    }
    throw createResponseError(raw, response.status, options);
  }

  if (contentType.toLowerCase().includes("application/json")) {
    const textFallback = cloneSmallJsonResponseForFallback(response);
    try {
      return (await response.json()) as T;
    } catch (error) {
      let raw = "";
      if (textFallback) {
        try {
          raw = await textFallback.text();
        } catch {
          throw error;
        }
      }
      if (looksLikeHtmlResponse(raw, contentType)) {
        throw createBackendUnavailableError(
          formatUnavailableApiMessage(path, response.status),
          response.status,
          { restartRequired: true },
        );
      }
      if (!raw) {
        if (looksLikeHtmlJsonParseError(error)) {
          throw createBackendUnavailableError(
            formatUnavailableApiMessage(path, response.status),
            response.status,
            { restartRequired: true },
          );
        }
        if (!textFallback) {
          throw error;
        }
        return {} as T;
      }
      throw error;
    }
  }

  const raw = await response.text();
  if (looksLikeHtmlResponse(raw, contentType)) {
    throw createBackendUnavailableError(
      formatUnavailableApiMessage(path, response.status),
      response.status,
      { restartRequired: true },
    );
  }
  return raw ? (JSON.parse(raw) as T) : ({} as T);
}

function cloneSmallJsonResponseForFallback(response: Response) {
  const contentLength = Number.parseInt(
    response.headers.get("content-length") ?? "",
    10,
  );
  if (
    !Number.isFinite(contentLength) ||
    contentLength < 0 ||
    contentLength > JSON_HTML_FALLBACK_TEXT_LIMIT_BYTES
  ) {
    return null;
  }
  return response.clone();
}

export async function performRequest(path: string, init?: RequestInit) {
  try {
    return await fetch(path, {
      headers: {
        "Content-Type": "application/json",
      },
      ...init,
    });
  } catch (error) {
    throw createBackendUnavailableError(
      "The TermAl backend is unavailable.",
      undefined,
      { cause: error },
    );
  }
}

export function createBackendUnavailableError(
  message: string,
  status?: number,
  options?: { restartRequired?: boolean; cause?: unknown },
) {
  return new ApiRequestError("backend-unavailable", message, {
    status,
    restartRequired: options?.restartRequired,
    cause: options?.cause,
  });
}

export function createResponseError(
  raw: string,
  status: number,
  options?: {
    preserveGatewayErrorBody?: boolean;
  },
) {
  if (status === 502 || status === 503 || status === 504) {
    if (options?.preserveGatewayErrorBody) {
      const gatewayError = extractIntentionalGatewayError(raw);
      if (gatewayError) {
        return new ApiRequestError("request-failed", gatewayError, {
          status,
        });
      }
    }
    return createBackendUnavailableError(
      "The TermAl backend is unavailable.",
      status,
    );
  }

  return new ApiRequestError("request-failed", extractError(raw, status), {
    status,
  });
}

function extractIntentionalGatewayError(raw: string) {
  const trimmed = raw.trim();
  if (trimmed.length === 0) {
    return null;
  }

  try {
    const parsed = JSON.parse(trimmed) as { error?: unknown };
    if (typeof parsed.error === "string" && parsed.error.trim().length > 0) {
      return sanitizeUserFacingErrorMessage(parsed.error);
    }
  } catch {
    return null;
  }

  return null;
}

function extractError(raw: string, status: number) {
  if (!raw) {
    return `Request failed with status ${status}.`;
  }

  try {
    const parsed = JSON.parse(raw) as { error?: string };
    if (parsed.error) {
      return sanitizeUserFacingErrorMessage(parsed.error);
    }
  } catch {
    return sanitizeUserFacingErrorMessage(raw);
  }

  return `Request failed with status ${status}.`;
}

export function looksLikeHtmlResponse(raw: string, contentType: string) {
  if (contentType.toLowerCase().includes("text/html")) {
    return true;
  }

  let start = 0;
  while (start < raw.length && /\s/.test(raw[start] ?? "")) {
    start += 1;
  }
  return (
    startsWithIgnoreCase(raw, start, "<!doctype html") ||
    startsWithIgnoreCase(raw, start, "<html")
  );
}

function looksLikeHtmlJsonParseError(error: unknown) {
  if (!(error instanceof SyntaxError)) {
    return false;
  }
  const message = error.message.toLowerCase();
  return (
    message.includes("unexpected token '<") ||
    message.includes("unexpected token <") ||
    message.includes("<!doctype") ||
    message.includes("<html")
  );
}

function startsWithIgnoreCase(value: string, start: number, prefix: string) {
  return value.slice(start, start + prefix.length).toLowerCase() === prefix;
}

export function formatUnavailableApiMessage(path: string, status: number) {
  const endpoint = path.split("?")[0] ?? path;
  const statusSuffix = status > 0 ? ` (HTTP ${status})` : "";
  return `The running backend does not expose ${endpoint}${statusSuffix}. Restart TermAl so the latest API routes are loaded.`;
}
