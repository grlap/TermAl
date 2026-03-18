import {
  siCss,
  siDart,
  siDocker,
  siGo,
  siHtml5,
  siJavascript,
  siJson,
  siMarkdown,
  siPython,
  siRust,
  siShell,
  siSqlite,
  siToml,
  siTypescript,
  siXml,
  siYaml,
  type SimpleIcon,
} from "simple-icons";

type FileLanguage =
  | "css"
  | "dart"
  | "dockerfile"
  | "go"
  | "html"
  | "ini"
  | "javascript"
  | "json"
  | "markdown"
  | "plaintext"
  | "python"
  | "rust"
  | "shell"
  | "sql"
  | "toml"
  | "typescript"
  | "xml"
  | "yaml";

type FileIconTone = "blue" | "gold" | "green" | "muted" | "red" | "rose";

type FileIconDescriptor = {
  icon: SimpleIcon | null;
  kind: FileLanguage | "generic";
  label: string;
  tone: FileIconTone;
};

const FILE_ICON_DESCRIPTORS: Record<FileLanguage, FileIconDescriptor> = {
  css: {
    icon: siCss,
    kind: "css",
    label: "CSS file",
    tone: "blue",
  },
  dart: {
    icon: siDart,
    kind: "dart",
    label: "Dart file",
    tone: "blue",
  },
  dockerfile: {
    icon: siDocker,
    kind: "dockerfile",
    label: "Dockerfile",
    tone: "blue",
  },
  go: {
    icon: siGo,
    kind: "go",
    label: "Go file",
    tone: "blue",
  },
  html: {
    icon: siHtml5,
    kind: "html",
    label: "HTML file",
    tone: "red",
  },
  ini: {
    icon: null,
    kind: "ini",
    label: "Config file",
    tone: "muted",
  },
  javascript: {
    icon: siJavascript,
    kind: "javascript",
    label: "JavaScript file",
    tone: "gold",
  },
  json: {
    icon: siJson,
    kind: "json",
    label: "JSON file",
    tone: "green",
  },
  markdown: {
    icon: siMarkdown,
    kind: "markdown",
    label: "Markdown file",
    tone: "rose",
  },
  plaintext: {
    icon: null,
    kind: "plaintext",
    label: "Text file",
    tone: "muted",
  },
  python: {
    icon: siPython,
    kind: "python",
    label: "Python file",
    tone: "gold",
  },
  rust: {
    icon: siRust,
    kind: "rust",
    label: "Rust file",
    tone: "red",
  },
  shell: {
    icon: siShell,
    kind: "shell",
    label: "Shell script",
    tone: "green",
  },
  sql: {
    icon: siSqlite,
    kind: "sql",
    label: "SQL file",
    tone: "blue",
  },
  toml: {
    icon: siToml,
    kind: "toml",
    label: "TOML file",
    tone: "green",
  },
  typescript: {
    icon: siTypescript,
    kind: "typescript",
    label: "TypeScript file",
    tone: "blue",
  },
  xml: {
    icon: siXml,
    kind: "xml",
    label: "XML file",
    tone: "rose",
  },
  yaml: {
    icon: siYaml,
    kind: "yaml",
    label: "YAML file",
    tone: "green",
  },
};

const GENERIC_FILE_ICON: FileIconDescriptor = {
  icon: null,
  kind: "generic",
  label: "File",
  tone: "muted",
};

export function FileTabIcon({
  className,
  language,
  path,
}: {
  className?: string;
  language?: string | null;
  path?: string | null;
}) {
  const descriptor = resolveFileIconDescriptor(language ?? null, path ?? null);
  const classNames = ["pane-tab-file-icon"];
  if (className) {
    classNames.push(className);
  }

  return (
    <span
      className={classNames.join(" ")}
      data-file-kind={descriptor.kind}
      data-file-tone={descriptor.tone}
      title={descriptor.label}
      aria-hidden="true"
    >
      {descriptor.icon ? (
        <svg className="pane-tab-file-icon-svg" viewBox="0 0 24 24" focusable="false" aria-hidden="true">
          <path d={descriptor.icon.path} fill="currentColor" />
        </svg>
      ) : (
        <GenericFileIcon />
      )}
    </span>
  );
}

function resolveFileIconDescriptor(language: string | null, path: string | null) {
  const normalizedLanguage = normalizeFileLanguage(language, path);
  return FILE_ICON_DESCRIPTORS[normalizedLanguage] ?? GENERIC_FILE_ICON;
}

function normalizeFileLanguage(language: string | null, path: string | null): FileLanguage {
  switch (language?.trim().toLowerCase()) {
    case "bash":
      return "shell";
    case "css":
    case "dart":
    case "dockerfile":
    case "go":
    case "html":
    case "ini":
    case "javascript":
    case "json":
    case "markdown":
    case "plaintext":
    case "python":
    case "rust":
    case "shell":
    case "sql":
    case "toml":
    case "typescript":
    case "xml":
    case "yaml":
      return language.trim().toLowerCase() as FileLanguage;
    default:
      return inferFileLanguageFromPath(path);
  }
}

function inferFileLanguageFromPath(path: string | null): FileLanguage {
  const normalized = path?.trim().toLowerCase();
  if (!normalized) {
    return "plaintext";
  }

  if (normalized.endsWith("/dockerfile") || normalized.endsWith("\\dockerfile")) {
    return "dockerfile";
  }

  if (normalized.endsWith("/makefile") || normalized.endsWith("\\makefile")) {
    return "plaintext";
  }

  if (normalized.endsWith(".css")) {
    return "css";
  }

  if (normalized.endsWith(".dart")) {
    return "dart";
  }

  if (
    normalized.endsWith(".js") ||
    normalized.endsWith(".jsx") ||
    normalized.endsWith(".mjs") ||
    normalized.endsWith(".cjs")
  ) {
    return "javascript";
  }

  if (
    normalized.endsWith(".ts") ||
    normalized.endsWith(".tsx") ||
    normalized.endsWith(".mts")
  ) {
    return "typescript";
  }

  if (normalized.endsWith(".json")) {
    return "json";
  }

  if (normalized.endsWith(".md") || normalized.endsWith(".mdx")) {
    return "markdown";
  }

  if (normalized.endsWith(".py")) {
    return "python";
  }

  if (normalized.endsWith(".rs")) {
    return "rust";
  }

  if (normalized.endsWith(".sql")) {
    return "sql";
  }

  if (normalized.endsWith(".toml")) {
    return "toml";
  }

  if (normalized.endsWith(".yaml") || normalized.endsWith(".yml")) {
    return "yaml";
  }

  if (
    normalized.endsWith(".html") ||
    normalized.endsWith(".htm") ||
    normalized.endsWith(".svg")
  ) {
    return "html";
  }

  if (normalized.endsWith(".xml")) {
    return "xml";
  }

  if (
    normalized.endsWith(".sh") ||
    normalized.endsWith(".bash") ||
    normalized.endsWith(".zsh")
  ) {
    return "shell";
  }

  if (normalized.endsWith(".ini")) {
    return "ini";
  }

  return "plaintext";
}

function GenericFileIcon() {
  return (
    <svg className="pane-tab-file-icon-svg pane-tab-file-icon-svg-generic" viewBox="0 0 16 16" focusable="false" aria-hidden="true">
      <path
        d="M4 1.75h5.1L12.75 5.4v8.85A1.75 1.75 0 0 1 11 16H4A1.75 1.75 0 0 1 2.25 14.25v-10.75A1.75 1.75 0 0 1 4 1.75Z"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.2"
        strokeLinejoin="round"
      />
      <path
        d="M9 1.9v2.85c0 .28.22.5.5.5h2.85"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.2"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <path
        d="M4.85 8.15h5.3M4.85 10.35h5.3M4.85 12.55h3.9"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.2"
        strokeLinecap="round"
      />
    </svg>
  );
}
