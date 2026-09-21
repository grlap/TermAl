# Local folder selection

Part of the [system architecture](../architecture.md#system-overview).

Project creation uses macOS `osascript` or native Windows `IFileOpenDialog`
folder mode. The dialog opens on the backend host's desktop, not on a remote
browser's machine. Manual entry remains available for other platforms and
hosts without an interactive desktop.

`windows_folder_picker.rs` owns a dedicated COM STA thread with per-monitor-v2
DPI awareness. A one-shot folder event queues normal restoration and requests
foreground activation. Windows focus policy may refuse foreground activation;
later folder navigation does not repeatedly raise the window. The thread's
previous DPI context and COM apartment are restored/released on exit.

Cancellation returns no selection. Selected filesystem directories still pass
through `paths.rs` validation. No shell command is used on Windows. Automated
tests cover COM creation, cancellation, Unicode paths, DPI restoration and
one-shot restoration; visible focus and mixed-monitor scaling require a desktop
smoke test after deployment.
