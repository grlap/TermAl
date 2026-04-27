// Pure helpers for resolving Markdown anchor `href` attributes into
// workspace-relative file-link targets that the UI can open in the
// source panel.
//
// What this file owns:
//   - `MarkdownFileLinkTarget` — the shape the caller receives and
//     passes to its `onOpenSourceLink` handler (path plus optional
//     line / column / openInNewTab).
//   - `resolveMarkdownFileLinkTarget` — the top-level resolver.
//     Takes a raw `href`, the workspace root, and the document's
//     own path, and returns a `{ path, line?, column? }` if the
//     href refers to a file inside the workspace, or `null` when
//     it's an external link, anchor, unresolvable relative path,
//     etc.
//   - `buildMarkdownHrefDisplayLabel` — produces a "nice"
//     workspace-relative display label for a link whose rendered
//     text mirrors the raw href (e.g. `/abs/path.md` becomes
//     `path.md#L42`).
//   - `transformMarkdownLinkUri` — small wrapper around the
//     `react-markdown` `uriTransformer` that lets local-file
//     hrefs through unchanged.
//   - A family of predicates / normalizers that support the
//     resolver: `normalizeMarkdownLocalFileHref`,
//     `isExternalMarkdownHref`, `safeDecodeMarkdownHref`,
//     `isMarkdownLocalFileUrl`,
//     `looksLikeAbsoluteHttpMarkdownFilePath`,
//     `isLoopbackMarkdownHostname`,
//     `looksLikeAbsoluteMarkdownFilePath`,
//     `normalizeMarkdownFileLinkAbsolutePath`,
//     `looksLikeRelativeMarkdownFilePath`,
//     `looksLikeFilePathReference`,
//     `parseMarkdownFileLinkFragment` (parses `#L42C3`),
//     `restoreMarkdownUncRootPrefix`,
//     `resolveMarkdownRelativeBasePath`,
//     `getMarkdownParentPath`, `joinWorkspacePath`,
//     `normalizeJoinedMarkdownPath`,
//     `formatMarkdownFileDisplayLocation`,
//     `extractMarkdownTextContent`.
//
// What this file does NOT own:
//   - The React components that render Markdown (`MarkdownContent`
//     and its siblings live in `./message-cards`).
//   - The `onOpenSourceLink` side effect — callers wire up their
//     own source-panel opener.
//   - Path-display primitives (`normalizeDisplayPath`,
//     `relativizePathToWorkspace`, `looksLikeWindowsPath`) live in
//     `./path-display`; this module composes them.
//
// Split out of `ui/src/message-cards.tsx`. Same signatures, same
// behaviour as the inline definitions they replaced; consumers
// (including `./panels/DiffPanel.tsx`, `./panels/SourcePanel.tsx`,
// `./MarkdownDocumentView.tsx`) import `MarkdownFileLinkTarget`
// from here directly.

import { isValidElement, type ReactNode } from "react";
import { uriTransformer } from "react-markdown";

import {
  looksLikeWindowsPath,
  normalizeDisplayPath,
  relativizePathToWorkspace,
} from "./path-display";

export type MarkdownFileLinkTarget = {
  path: string;
  line?: number;
  column?: number;
  openInNewTab?: boolean;
};

export const MARKDOWN_INTERNAL_LINK_HREF_ATTRIBUTE = "data-markdown-link-href";

export function resolveMarkdownFileLinkTarget(
  href: string | undefined,
  workspaceRoot: string | null,
  documentPath: string | null = null,
): Omit<MarkdownFileLinkTarget, "openInNewTab"> | null {
  const normalizedHref = normalizeMarkdownLocalFileHref(href);
  if (!normalizedHref || isExternalMarkdownHref(normalizedHref) || normalizedHref.startsWith("#")) {
    return null;
  }

  let candidate = safeDecodeMarkdownHref(normalizedHref).replace(/^file:\/\//i, "");
  let line: number | undefined;
  let column: number | undefined;

  const hashIndex = candidate.indexOf("#");
  if (hashIndex >= 0) {
    const fragment = candidate.slice(hashIndex + 1);
    candidate = candidate.slice(0, hashIndex);
    const fragmentLocation = parseMarkdownFileLinkFragment(fragment);
    if (fragmentLocation) {
      line = fragmentLocation.line;
      column = fragmentLocation.column;
    }
  }

  const lineSuffixMatch = candidate.match(/^(.*?)(?::(\d+)(?::(\d+))?)$/);
  if (lineSuffixMatch) {
    candidate = lineSuffixMatch[1] ?? candidate;
    if (!line && lineSuffixMatch[2]) {
      line = Number(lineSuffixMatch[2]);
    }
    if (!column && lineSuffixMatch[3]) {
      column = Number(lineSuffixMatch[3]);
    }
  }

  let trimmedCandidate = candidate.trim();
  if ((line || column) && trimmedCandidate.endsWith(".")) {
    trimmedCandidate = trimmedCandidate.slice(0, -1).trimEnd();
  }
  if (!trimmedCandidate) {
    return null;
  }

  const resolvedPath = looksLikeAbsoluteMarkdownFilePath(trimmedCandidate, workspaceRoot)
    ? normalizeMarkdownFileLinkAbsolutePath(trimmedCandidate)
    : !workspaceRoot || !looksLikeRelativeMarkdownFilePath(trimmedCandidate)
      ? null
      : joinWorkspacePath(resolveMarkdownRelativeBasePath(workspaceRoot, documentPath), trimmedCandidate);
  if (!resolvedPath) {
    return null;
  }

  return {
    path: restoreMarkdownUncRootPrefix(resolvedPath, workspaceRoot),
    ...(line ? { line } : {}),
    ...(column ? { column } : {}),
  };
}

export function restoreMarkdownUncRootPrefix(path: string, workspaceRoot: string | null) {
  const trimmedRoot = workspaceRoot?.trim() ?? "";
  if (!/^\\\\/.test(trimmedRoot) || /^\\\\/.test(path)) {
    return path;
  }

  const uncShare = trimmedRoot.replace(/^\\\\/, "").split(/[\\/]+/).slice(0, 2).join("\\");
  const trimmedPath = path.replace(/^[\\/]+/, "");
  return uncShare && trimmedPath.toLowerCase().startsWith(`${uncShare.toLowerCase()}\\`)
    ? `\\\\${trimmedPath}`
    : path;
}

export function parseMarkdownFileLinkFragment(fragment: string) {
  const match = fragment.trim().match(/^L(\d+)(?:C(\d+))?$/i);
  if (!match) {
    return null;
  }

  return {
    line: Number(match[1]),
    ...(match[2] ? { column: Number(match[2]) } : {}),
  };
}

export function isExternalMarkdownHref(href: string) {
  const normalizedHref = normalizeMarkdownLocalFileHref(href);
  if (!normalizedHref) {
    return false;
  }
  const decodedHref = safeDecodeMarkdownHref(normalizedHref);

  if (
    /^file:\/\//i.test(normalizedHref) ||
    /^\/[A-Za-z]:[\\/]/.test(normalizedHref) ||
    /^[A-Za-z]:[\\/]/.test(normalizedHref) ||
    /^\/[A-Za-z]:[\\/]/.test(decodedHref) ||
    /^[A-Za-z]:[\\/]/.test(decodedHref)
  ) {
    return false;
  }

  return /^\/\//.test(normalizedHref) || /^[a-z][a-z\d+.-]*:/i.test(normalizedHref);
}

export function transformMarkdownLinkUri(href: string) {
  if (!isExternalMarkdownHref(href)) {
    return href;
  }
  const transformed = uriTransformer(href);
  // `react-markdown`'s `uriTransformer` neutralizes dangerous
  // protocols (`javascript:`, `vbscript:`, most `data:` URIs) by
  // substituting the literal string `"javascript:void(0)"`. React
  // ≥18.3 emits a console warning every time that string reaches
  // the DOM and is slated to block it entirely in a future version.
  // Swap the placeholder for an empty string so React doesn't see a
  // `javascript:` URL at all; the `a` renderer in `./message-cards`
  // renders an empty href as a plain `<span>` so there's no inert
  // same-page-navigate behaviour either.
  return transformed === "javascript:void(0)" ? "" : transformed;
}

export function shouldScrubMarkdownDomHref(href: string | undefined) {
  const normalizedHref = normalizeMarkdownLocalFileHref(href);
  if (!normalizedHref) {
    return false;
  }

  const decodedHref = safeDecodeMarkdownHref(normalizedHref);
  return (
    /^file:\/\//i.test(normalizedHref) ||
    /^\/[A-Za-z]:[\\/]/.test(normalizedHref) ||
    /^[A-Za-z]:[\\/]/.test(normalizedHref) ||
    /^\/[A-Za-z]:[\\/]/.test(decodedHref) ||
    /^[A-Za-z]:[\\/]/.test(decodedHref)
  );
}

export function safeDecodeMarkdownHref(href: string) {
  try {
    return decodeURIComponent(href);
  } catch {
    return href;
  }
}

export function normalizeMarkdownLocalFileHref(href: string | undefined) {
  const trimmedHref = href?.trim();
  if (!trimmedHref) {
    return null;
  }

  let parsedHref: URL;
  try {
    parsedHref = new URL(trimmedHref);
  } catch {
    return trimmedHref;
  }

  if (!isMarkdownLocalFileUrl(parsedHref)) {
    return trimmedHref;
  }

  const decodedPathname = safeDecodeMarkdownHref(parsedHref.pathname);
  if (!looksLikeAbsoluteHttpMarkdownFilePath(decodedPathname)) {
    return trimmedHref;
  }

  const normalizedPath = normalizeMarkdownFileLinkAbsolutePath(decodedPathname);
  if (!looksLikeAbsoluteMarkdownFilePath(normalizedPath, null)) {
    return trimmedHref;
  }

  return `${normalizedPath}${parsedHref.hash}`;
}

export function isMarkdownLocalFileUrl(url: URL) {
  return /^https?:$/i.test(url.protocol) && isLoopbackMarkdownHostname(url.hostname);
}

export function looksLikeAbsoluteHttpMarkdownFilePath(pathname: string) {
  return /^\/[A-Za-z]:[\\/]/.test(pathname) ||
    /^\/(?:Users|home|root|tmp|var|private|opt|usr|etc|srv|mnt|Volumes)\//.test(pathname);
}

export function isLoopbackMarkdownHostname(hostname: string) {
  const normalizedHostname = hostname.trim().replace(/^\[/, "").replace(/\]$/, "").toLowerCase();
  return (
    normalizedHostname === "localhost" ||
    normalizedHostname === "127.0.0.1" ||
    normalizedHostname === "0.0.0.0" ||
    normalizedHostname === "::1"
  );
}

export function buildMarkdownHrefDisplayLabel(
  href: string | undefined,
  children: ReactNode,
  workspaceRoot: string | null,
  documentPath: string | null = null,
) {
  const trimmedHref = href?.trim();
  const renderedText = extractMarkdownTextContent(children).trim();
  if (!trimmedHref || !renderedText || renderedText !== trimmedHref) {
    return null;
  }

  const normalizedHref = normalizeMarkdownLocalFileHref(trimmedHref);
  if (!normalizedHref || normalizedHref === trimmedHref) {
    return null;
  }

  const fileLinkTarget = resolveMarkdownFileLinkTarget(normalizedHref, workspaceRoot, documentPath);
  if (!fileLinkTarget) {
    return null;
  }

  const displayPath = normalizeDisplayPath(relativizePathToWorkspace(fileLinkTarget.path, workspaceRoot));
  return `${displayPath}${formatMarkdownFileDisplayLocation(fileLinkTarget.line, fileLinkTarget.column)}`;
}

export function extractMarkdownTextContent(node: ReactNode): string {
  if (typeof node === "string" || typeof node === "number") {
    return String(node);
  }

  if (Array.isArray(node)) {
    return node.map((child) => extractMarkdownTextContent(child)).join("");
  }

  if (isValidElement<{ children?: ReactNode }>(node)) {
    return extractMarkdownTextContent(node.props.children ?? null);
  }

  return "";
}

export function formatMarkdownFileDisplayLocation(line: number | undefined, column: number | undefined) {
  if (!line) {
    return "";
  }

  return column ? `#L${line}C${column}` : `#L${line}`;
}

export function looksLikeAbsoluteMarkdownFilePath(path: string, workspaceRoot: string | null) {
  if (/^\/[A-Za-z]:[\\/]/.test(path) || /^[A-Za-z]:[\\/]/.test(path) || /^\\\\/.test(path)) {
    return true;
  }

  if (!path.startsWith("/") || path.startsWith("//")) {
    return false;
  }

  const trimmedRoot = workspaceRoot?.trim();
  if (trimmedRoot) {
    const normalizedPath = normalizeDisplayPath(path);
    const normalizedRoot = normalizeDisplayPath(trimmedRoot).replace(/\/+$/, "");
    if (normalizedPath === normalizedRoot || normalizedPath.startsWith(`${normalizedRoot}/`)) {
      return true;
    }
  }

  return looksLikeFilePathReference(path);
}

export function normalizeMarkdownFileLinkAbsolutePath(path: string) {
  return /^\/[A-Za-z]:[\\/]/.test(path) ? path.slice(1) : path;
}

export function looksLikeRelativeMarkdownFilePath(path: string) {
  if (!path || path.startsWith("/") || /^[a-z][a-z\d+.-]*:/i.test(path)) {
    return false;
  }

  return path.startsWith("./") || path.startsWith("../") || /[\\/]/.test(path) || looksLikeFilePathReference(path);
}

export function looksLikeFilePathReference(path: string) {
  const segments = path.trim().split(/[\\/]+/).filter(Boolean);
  const fileName = segments[segments.length - 1] ?? "";
  return fileName.startsWith(".") || fileName.includes(".");
}

export function resolveMarkdownRelativeBasePath(workspaceRoot: string, documentPath: string | null) {
  const trimmedDocumentPath = documentPath?.trim();
  if (!trimmedDocumentPath) {
    return workspaceRoot;
  }

  if (looksLikeAbsoluteMarkdownFilePath(trimmedDocumentPath, workspaceRoot)) {
    return getMarkdownParentPath(trimmedDocumentPath) ?? workspaceRoot;
  }

  const relativeParent = getMarkdownParentPath(trimmedDocumentPath);
  return relativeParent ? joinWorkspacePath(workspaceRoot, relativeParent) : workspaceRoot;
}

export function getMarkdownParentPath(path: string) {
  const trimmedPath = path.trim().replace(/[\\/]+$/, "");
  if (!trimmedPath) {
    return null;
  }

  const slashIndex = Math.max(trimmedPath.lastIndexOf("/"), trimmedPath.lastIndexOf("\\"));
  if (slashIndex <= 0) {
    return null;
  }

  return trimmedPath.slice(0, slashIndex);
}

export function joinWorkspacePath(rootPath: string, relativePath: string) {
  const trimmedRoot = rootPath.trim().replace(/[\\/]+$/, "");
  const trimmedRelative = relativePath.trim().replace(/^[\\/]+/, "");
  if (!trimmedRoot) {
    return trimmedRelative;
  }

  if (!trimmedRelative) {
    return trimmedRoot;
  }

  const useBackslashSeparator = looksLikeWindowsPath(trimmedRoot);
  const normalizedRelative = useBackslashSeparator
    ? trimmedRelative.replace(/\//g, "\\")
    : trimmedRelative.replace(/\\/g, "/");
  return normalizeJoinedMarkdownPath(
    `${trimmedRoot}${useBackslashSeparator ? "\\" : "/"}${normalizedRelative}`,
    useBackslashSeparator ? "\\" : "/",
  );
}

export function normalizeJoinedMarkdownPath(path: string, separator: "\\" | "/") {
  const isWindowsPath = separator === "\\";
  const normalizedPath = isWindowsPath ? path.replace(/\//g, "\\") : path.replace(/\\/g, "/");
  let prefix = "";
  let rest = normalizedPath;
  const driveMatch = isWindowsPath ? normalizedPath.match(/^([A-Za-z]:)\\?(.*)$/) : null;
  const uncMatch = isWindowsPath ? normalizedPath.match(/^\\\\([^\\]+)\\([^\\]+)\\?(.*)$/) : null;

  if (uncMatch) {
    prefix = `\\\\${uncMatch[1]}\\${uncMatch[2]}\\`;
    rest = uncMatch[3] ?? "";
  } else if (driveMatch) {
    prefix = `${driveMatch[1]}\\`;
    rest = driveMatch[2] ?? "";
  } else if (!isWindowsPath && normalizedPath.startsWith("/")) {
    prefix = "/";
    rest = normalizedPath.slice(1);
  }

  const segments: string[] = [];
  for (const segment of rest.split(separator)) {
    if (!segment || segment === ".") {
      continue;
    }
    if (segment === "..") {
      if (segments.length > 0 && segments[segments.length - 1] !== "..") {
        segments.pop();
      } else if (!prefix) {
        segments.push(segment);
      }
      continue;
    }
    segments.push(segment);
  }

  return `${prefix}${segments.join(separator)}`;
}
