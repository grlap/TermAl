import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { updateEngramHostSettings, type StateResponse } from "../api";
import type { Project } from "../types";
import { EngramPreferencesPanel } from "./engram-preferences-panel";

vi.mock("../api", async (importOriginal) => {
  const original = await importOriginal<typeof import("../api")>();
  return {
    ...original,
    updateEngramHostSettings: vi.fn(),
  };
});

const mockUpdateEngramHostSettings = vi.mocked(updateEngramHostSettings);

const declaredProject: Project = {
  id: "declared",
  name: "Declared repo",
  rootPath: "C:\\work\\declared",
  remoteId: "local",
  engramDeclared: true,
  engramGrantConfigured: false,
  engramOperatorDisabled: false,
};

beforeEach(() => {
  vi.clearAllMocks();
  mockUpdateEngramHostSettings.mockResolvedValue({} as StateResponse);
});

describe("EngramPreferencesPanel", () => {
  it("saves host paths and lists only repository-declared projects", async () => {
    const onStateUpdated = vi.fn();
    render(
      <EngramPreferencesPanel
        hostSettings={{ binaryPath: "engram", home: "C:\\Engram" }}
        projects={[
          declaredProject,
          {
            ...declaredProject,
            id: "undeclared",
            name: "Undeclared repo",
            engramDeclared: false,
          },
        ]}
        onStateUpdated={onStateUpdated}
      />,
    );

    expect(screen.getByLabelText("Engram project")).toHaveValue("declared");
    expect(screen.queryByText(/Undeclared repo/)).not.toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("Engram host binary path"), {
      target: { value: "C:\\tools\\engram.exe" },
    });
    fireEvent.change(screen.getByLabelText("Engram host home"), {
      target: { value: "C:\\EngramHome" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save host settings" }));

    await waitFor(() =>
      expect(mockUpdateEngramHostSettings).toHaveBeenCalledWith({
        binaryPath: "C:\\tools\\engram.exe",
        home: "C:\\EngramHome",
      }),
    );
    expect(onStateUpdated).toHaveBeenCalledTimes(1);
  });

  it("explains that an undeclared repository is not configurable", () => {
    render(
      <EngramPreferencesPanel
        hostSettings={{ binaryPath: "engram", home: "C:\\Engram" }}
        projects={[{ ...declaredProject, engramDeclared: false }]}
        onStateUpdated={vi.fn()}
      />,
    );

    expect(
      screen.getByText(/No local project declares Engram yet/),
    ).toBeInTheDocument();
    expect(screen.queryByLabelText("Engram project")).not.toBeInTheDocument();
  });
});
