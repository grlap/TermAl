import {
  highlightCode,
  inferLanguageFromCommand,
  inferLanguageFromPath,
  normalizeLanguage,
} from "./highlight";

describe("highlight helpers", () => {
  it("normalizes supported aliases", () => {
    expect(normalizeLanguage("tsx")).toBe("typescript");
    expect(normalizeLanguage("zsh")).toBe("bash");
    expect(normalizeLanguage("language-json")).toBe("json");
    expect(normalizeLanguage("dart")).toBe("dart");
  });

  it("infers languages from file paths", () => {
    expect(inferLanguageFromPath("ui/src/App.tsx")).toBe("typescript");
    expect(inferLanguageFromPath("/tmp/Cargo.toml")).toBe("ini");
    expect(inferLanguageFromPath("README.md")).toBe("markdown");
    expect(inferLanguageFromPath("lib/main.dart")).toBe("dart");
  });

  it("infers command output language from file-viewing commands", () => {
    expect(inferLanguageFromCommand(`/bin/zsh -lc "sed -n '1,120p' ui/src/App.tsx"`)).toBe(
      "typescript",
    );
    expect(inferLanguageFromCommand("git diff -- ui/src/App.tsx")).toBe("diff");
    expect(inferLanguageFromCommand("npm test")).toBeNull();
  });

  it("highlights known languages and falls back to escaped plain text", () => {
    expect(highlightCode("const value = 1;", { language: "ts" }).language).toBe("typescript");
    expect(highlightCode("void main() {}", { language: "dart" }).language).toBe("dart");
    expect(
      highlightCode("<script>alert('x')</script>", {
        language: "not-real",
      }).html,
    ).toContain("&lt;script&gt;");
  });
});
