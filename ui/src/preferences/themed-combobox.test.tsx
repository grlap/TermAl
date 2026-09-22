import { fireEvent, render, screen } from "@testing-library/react";
import { expect, it, vi } from "vitest";
import { ThemedCombobox } from "./themed-combobox";

const options = [
  { value: "auto", label: "Auto" },
  { value: "unavailable", label: "Unavailable", disabled: true },
  { value: "supported", label: "Supported" },
];

it("exposes disabled choices without allowing pointer or keyboard selection", () => {
  const onChange = vi.fn();
  render(<ThemedCombobox id="mode" aria-label="Mode" options={options} value="auto" onChange={onChange} />);
  fireEvent.click(screen.getByRole("combobox"));
  const unavailable = screen.getByRole("option", { name: "Unavailable" });
  expect(unavailable).toBeDisabled();
  expect(unavailable).toHaveAttribute("aria-disabled", "true");
  fireEvent.click(unavailable);
  const trigger = screen.getByRole("combobox");
  trigger.focus();
  fireEvent.keyDown(trigger, { key: "ArrowDown" });
  fireEvent.keyDown(trigger, { key: "Enter" });
  expect(onChange).toHaveBeenCalledExactlyOnceWith("supported");
  expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
});

it("advances repeatedly from the focused trigger and skips disabled options", () => {
  const onChange = vi.fn();
  render(<ThemedCombobox id="mode" aria-label="Mode" options={options} value="auto" onChange={onChange} />);
  const trigger = screen.getByRole("combobox");
  trigger.focus();
  fireEvent.keyDown(trigger, { key: "ArrowDown" });
  fireEvent.keyDown(trigger, { key: "ArrowDown" });
  expect(trigger).toHaveAttribute("aria-activedescendant", expect.stringMatching(/option-2$/));
  fireEvent.keyDown(trigger, { key: "ArrowUp" });
  expect(trigger).toHaveAttribute("aria-activedescendant", expect.stringMatching(/option-0$/));
  fireEvent.keyDown(trigger, { key: "End" });
  fireEvent.keyDown(trigger, { key: "Enter" });
  expect(onChange).toHaveBeenCalledExactlyOnceWith("supported");
});

it("starts missing and disabled selections on an enabled option", () => {
  const onChange = vi.fn();
  const view = render(<ThemedCombobox id="mode" aria-label="Mode" options={options} value="missing" onChange={onChange} />);
  const trigger = screen.getByRole("combobox");
  fireEvent.keyDown(trigger, { key: "ArrowDown" });
  fireEvent.keyDown(trigger, { key: "Enter" });
  expect(onChange).toHaveBeenCalledExactlyOnceWith("auto");

  onChange.mockClear();
  view.rerender(<ThemedCombobox id="mode" aria-label="Mode" options={options} value="unavailable" onChange={onChange} />);
  fireEvent.keyDown(trigger, { key: "ArrowDown" });
  fireEvent.keyDown(trigger, { key: "Home" });
  fireEvent.keyDown(trigger, { key: "Enter" });
  expect(onChange).toHaveBeenCalledExactlyOnceWith("auto");
});

it("keeps empty and all-disabled controls inert under navigation", () => {
  const onChange = vi.fn();
  const view = render(<ThemedCombobox id="mode" aria-label="Mode" options={[]} value="" onChange={onChange} />);
  const trigger = screen.getByRole("combobox");
  for (const key of ["ArrowDown", "ArrowUp", "Home", "End", "Enter", " "]) {
    fireEvent.keyDown(trigger, { key });
  }
  expect(trigger).toHaveAttribute("aria-expanded", "false");
  view.rerender(<ThemedCombobox id="mode" aria-label="Mode" options={[{ value: "off", label: "Off", disabled: true }]} value="off" onChange={onChange} />);
  for (const key of ["ArrowDown", "ArrowUp", "Home", "End", "Enter"]) {
    fireEvent.keyDown(trigger, { key });
  }
  expect(trigger).toHaveAttribute("aria-expanded", "false");
  expect(onChange).not.toHaveBeenCalled();
});

it("closes the portal and blocks selection when an open control becomes disabled", () => {
  const onChange = vi.fn();
  const props = { id: "mode", "aria-label": "Mode", options, value: "auto", onChange };
  const view = render(<ThemedCombobox {...props} />);
  fireEvent.click(screen.getByRole("combobox"));
  expect(screen.getByRole("listbox")).toHaveClass("combo-menu");
  view.rerender(<ThemedCombobox {...props} disabled />);
  expect(screen.getByRole("combobox")).toBeDisabled();
  expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
  fireEvent.keyDown(window, { key: "End" });
  fireEvent.keyDown(window, { key: "Enter" });
  expect(onChange).not.toHaveBeenCalled();
});
