# TermAl product website

This directory is a dependency-free static product site for TermAl. It is intentionally isolated from the Rust backend and the React application in `ui/`.

## Preview

From the repository root:

```powershell
python -m http.server 8080 --directory website
```

Then open `http://localhost:8080/`.

Run the static contract checks with:

```powershell
node website/verify.mjs
```

The page works without JavaScript. JavaScript adds the guided control-room demo, scroll reveals, theme preview, copy buttons, progress state, and mobile-menu focus management. Visitors who request reduced motion get the completed workflow state without timed movement.

## Publishing boundary

Deploy only the contents of `website/`; this directory is not part of the TermAl runtime.

The current document deliberately includes `noindex,nofollow`. Before a public launch, the repository owner should:

1. Decide and publish the project's license or other source-use terms.
2. Choose the production URL and add a canonical URL, `og:url`, an absolute social image, and a sitemap.
3. Change the robots directive only after those decisions are final.
4. Run `node website/verify.mjs --launch` as a final release gate.

The launch-mode verifier is expected to fail until those prerequisites exist.

## Product-copy guardrails

- TermAl currently integrates Claude Code, OpenAI Codex, Gemini CLI, and Cursor Agent.
- It runs from source; no packaged release is advertised.
- “Source available” is intentional. Do not change it to “open source” until a license is declared.
- Approval and sandbox controls vary by agent and configuration. Do not promise that every command is sandboxed or requires approval.
- Multi-browser layouts are separate persisted views, not collaborative multi-user editing.
- Never advise exposing the local API on port `8787` to an untrusted network.

