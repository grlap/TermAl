# Mermaid Demo

```mermaid
flowchart TD
  Start([Open Markdown file]) --> Detect{Contains Mermaid fence?}
  Detect -- yes --> Render[Render diagram]
  Detect -- no --> Code[Show normal code block]
  Render --> Edit[Edit source in Markdown diff editor]
  Code --> End2([Done])
  Edit --> Stop
```
