import { describe, expect, it } from "vitest";

import { formatUserFacingError, sanitizeUserFacingErrorMessage } from "./error-messages";

describe("error messages", () => {
  it("sanitizes remote SSH startup failures", () => {
    expect(
      sanitizeUserFacingErrorMessage(
        "failed to reach remote `pop-os` over SSH. managed start failed: managed SSH session exited with exit code: 255: ssh: Could not resolve hostname pop-os.local: No such host is known.. tunnel-only fallback failed: SSH tunnel exited with exit code: 255: ssh: Could not resolve hostname pop-os.local: No such host is known.",
      ),
    ).toBe(
      'Could not connect to remote "pop-os" over SSH. Check the host, network, and SSH settings, then try again.',
    );
  });

  it("sanitizes remote contact failures that include host details", () => {
    expect(
      sanitizeUserFacingErrorMessage(
        "failed to contact remote `pop-os` at pop-os.local: error sending request for url (http://127.0.0.1:47001/api/state): connection refused",
      ),
    ).toBe(
      'Could not connect to remote "pop-os" over SSH. Check the host, network, and SSH settings, then try again.',
    );
  });

  it("leaves safe validation errors unchanged", () => {
    expect(sanitizeUserFacingErrorMessage("remote `SSH Lab` has an invalid SSH host")).toBe(
      "remote `SSH Lab` has an invalid SSH host",
    );
  });

  it("sanitizes local SSH client startup failures separately", () => {
    expect(
      sanitizeUserFacingErrorMessage(
        "failed to start SSH connection for remote `pop-os`: CreateProcessW failed",
      ),
    ).toBe(
      'Could not start the local SSH client for remote "pop-os". Verify OpenSSH is installed and available on PATH, then try again.',
    );
  });

  it("formats thrown errors through the same sanitizer", () => {
    expect(
      formatUserFacingError(
        new Error("failed to start SSH connection for remote `pop-os`: CreateProcessW failed"),
      ),
    ).toBe(
      'Could not start the local SSH client for remote "pop-os". Verify OpenSSH is installed and available on PATH, then try again.',
    );
  });
});
