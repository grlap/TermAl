// Searchable native-checkbox disclosure, not a faux listbox. Counts describe
// the loaded snapshot before local kind/label filtering, and selections stay
// visible at zero count after refresh. No source reads are triggered here.
import { useEffect, useId, useMemo, useRef, useState } from "react";
import type { WorkItem } from "../work-visualizer-api";
import { workLabelCounts, type WorkLabelMode } from "./work-labels";

export function WorkLabelPicker({ rows, selected, onChange, mode, onModeChange }: {
  rows: readonly WorkItem[]; selected: readonly string[]; onChange: (labels: string[]) => void;
  mode: WorkLabelMode; onModeChange: (mode: WorkLabelMode) => void;
}) {
  const [open, setOpen] = useState(false);
  const [search, setSearch] = useState("");
  const root = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const searchInput = useRef<HTMLInputElement>(null);
  const id = useId();
  const counts = useMemo(() => workLabelCounts(rows), [rows]);
  const options = [...new Set([...counts.keys(), ...selected])].sort((a, b) => a.localeCompare(b))
    .filter(label => label.toLocaleLowerCase().includes(search.toLocaleLowerCase()));
  useEffect(() => {
    if (!open) return;
    searchInput.current?.focus();
    const outside = (event: PointerEvent) => { if (event.target instanceof Node && !root.current?.contains(event.target)) setOpen(false); };
    document.addEventListener("pointerdown", outside);
    return () => document.removeEventListener("pointerdown", outside);
  }, [open]);
  const close = () => { setOpen(false); trigger.current?.focus(); };
  return <div className="work-label-filter" ref={root} onKeyDown={event => {
    if (open && event.key === "Escape") { event.preventDefault(); event.stopPropagation(); close(); }
  }} onBlur={event => { if (event.relatedTarget instanceof Node && !event.currentTarget.contains(event.relatedTarget)) setOpen(false); }}>
    <button ref={trigger} type="button" aria-expanded={open} aria-controls={id} onClick={() => { setSearch(""); setOpen(!open); }}>
      Labels (loaded rows){selected.length ? ` · ${selected.length} selected` : " · All"} ▾
    </button>
    {open && <div id={id} className="work-label-picker" role="group" aria-label="Choose labels">
      <label>Search labels <input ref={searchInput} value={search} onChange={event => setSearch(event.target.value)} /></label>
      <div className="work-panel-actions" role="group" aria-label="Match labels">
        <button type="button" aria-pressed={mode === "any"} onClick={() => onModeChange("any")}>Any label</button>
        <button type="button" aria-pressed={mode === "all"} onClick={() => onModeChange("all")}>All labels</button>
      </div>
      <p className="work-panel-caption">Counts from all {rows.length} loaded items.</p>
      <div className="work-label-options">{options.map(label => <label key={label}>
        <input type="checkbox" aria-label={label || "(empty label)"} aria-description={`${counts.get(label) ?? 0} loaded items`} checked={selected.includes(label)} onChange={() => onChange(selected.includes(label)
          ? selected.filter(value => value !== label) : [...selected, label])} />
        <span>{label || "(empty label)"}</span><span className="work-label-count">{counts.get(label) ?? 0}</span>
      </label>)}{!options.length && <p>No matching labels in loaded items.</p>}</div>
      <div className="work-panel-actions"><button type="button" onClick={() => onChange([])}>Clear labels</button><button type="button" onClick={close}>Done</button></div>
    </div>}
    {selected.length > 0 && <div className="work-label-chips" aria-label="Selected labels">
      {selected.map(label => <button type="button" className="work-label-chip" key={label} aria-label={`Remove label ${label}`} onClick={() => onChange(selected.filter(value => value !== label))}>{label || "(empty label)"} ×</button>)}
      <button type="button" className="work-label-chip" onClick={() => onChange([])}>Clear labels</button>
    </div>}
  </div>;
}
