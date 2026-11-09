import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { createRef } from "react";
import { afterEach, expect, it, vi } from "vitest";
import { ThemedCombobox } from "./themed-combobox";

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

const options = [
  { value: "auto", label: "Auto" },
  { value: "unavailable", label: "Unavailable", disabled: true },
  { value: "supported", label: "Supported" },
];

it("anchors an open menu to its panel, follows pane resizing and preserves keyboard and pointer selection", () => {
  const anchor = createRef<HTMLDivElement>();
  let panelRect = new DOMRect(40, 20, 440, 80);
  const triggerRect = new DOMRect(40, 40, 210, 30);
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
    return this === anchor.current ? panelRect : this.id === "project" ? triggerRect : new DOMRect();
  });
  let resized = () => {};
  const observe = vi.fn();
  const disconnect = vi.fn();
  vi.stubGlobal("ResizeObserver", class {
    constructor(callback: () => void) { resized = callback; }
    observe = observe;
    disconnect = disconnect;
  });
  const onChange = vi.fn();
  render(<div ref={anchor}><ThemedCombobox id="project" aria-label="Project" menuAnchorRef={anchor}
    options={options} value="auto" onChange={onChange} /></div>);
  const trigger = screen.getByRole("combobox");
  fireEvent.click(trigger);
  const menu = screen.getByRole("listbox");
  expect(menu.parentElement).toBe(document.body);
  expect(menu).toHaveStyle({ left: "40px", width: "440px", top: "78px" });
  expect(observe).toHaveBeenCalledWith(anchor.current);
  expect(observe).toHaveBeenCalledWith(trigger);
  panelRect = new DOMRect(60, 20, 320, 80);
  act(() => resized());
  expect(menu).toHaveStyle({ left: "60px", width: "320px" });
  fireEvent.keyDown(trigger, { key: "ArrowDown" });
  fireEvent.keyDown(trigger, { key: "Enter" });
  expect(onChange).toHaveBeenLastCalledWith("supported");
  expect(trigger).toHaveFocus();
  expect(disconnect).toHaveBeenCalledTimes(1);
  fireEvent.click(trigger);
  fireEvent.click(screen.getByRole("option", { name: "Supported" }));
  expect(onChange).toHaveBeenCalledTimes(2);
  expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
});

it("keeps an anchored menu inside the viewport and leaves default menus sized to their trigger", () => {
  const anchor = createRef<HTMLDivElement>();
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(function (this: HTMLElement) {
    return this === anchor.current ? new DOMRect(-20, 0, window.innerWidth + 80, 100) : new DOMRect(20, 20, 180, 30);
  });
  vi.stubGlobal("ResizeObserver", class { observe() {} disconnect() {} });
  const props = { id: "mode", options, value: "auto", onChange: vi.fn() };
  const view = render(<div ref={anchor}><ThemedCombobox {...props} menuAnchorRef={anchor} /></div>);
  fireEvent.click(screen.getByRole("combobox"));
  expect(screen.getByRole("listbox")).toHaveStyle({ left: "0px", width: `${window.innerWidth}px` });
  fireEvent.keyDown(window, { key: "Escape" });
  view.rerender(<div ref={anchor}><ThemedCombobox {...props} /></div>);
  fireEvent.click(screen.getByRole("combobox"));
  expect(screen.getByRole("listbox")).toHaveStyle({ left: "20px", width: "180px" });
});

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
