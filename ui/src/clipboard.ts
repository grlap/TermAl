export async function copyTextToClipboard(text: string): Promise<void> {
  if (typeof navigator !== "undefined" && navigator.clipboard?.writeText) {
    try {
      await navigator.clipboard.writeText(text);
      return;
    } catch {
      // Fall through to the document-based copy path when the Clipboard API
      // exists but rejects due to browser permissions or context constraints.
    }
  }

  if (typeof document === "undefined") {
    throw new Error("Clipboard is unavailable.");
  }

  if (typeof document.execCommand !== "function") {
    throw new Error("Clipboard is unavailable.");
  }

  const textarea = document.createElement("textarea");
  textarea.value = text;
  textarea.setAttribute("readonly", "");
  textarea.style.position = "fixed";
  textarea.style.top = "0";
  textarea.style.left = "0";
  textarea.style.opacity = "0";
  textarea.style.pointerEvents = "none";
  textarea.style.contain = "strict";
  document.body.appendChild(textarea);

  const selection = document.getSelection();
  const previousRange =
    selection && selection.rangeCount > 0 ? selection.getRangeAt(0).cloneRange() : null;

  textarea.focus();
  textarea.select();
  textarea.setSelectionRange(0, textarea.value.length);

  const didCopy = document.execCommand("copy");
  document.body.removeChild(textarea);

  if (selection) {
    selection.removeAllRanges();
    if (previousRange) {
      selection.addRange(previousRange);
    }
  }

  if (!didCopy) {
    throw new Error("Clipboard copy failed.");
  }
}
