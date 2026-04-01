# Feature Brief: Orchestration

Backlog source: [`docs/bugs.md`](../bugs.md)

## The problem

TermAl can already run many agent sessions in parallel, but the developer still does the routing:
read a session's reply, decide who should act next, copy the result into the next prompt, and keep
switching tabs. That is coordination work, not product work.

The missing piece is orchestration: a reusable graph of sessions and transitions that lets TermAl
move work forward automatically once a session finishes its turn and is ready for the next prompt.

## Core model

An orchestration template is a graph:

- Nodes are regular TermAl sessions.
- Edges are transitions between sessions.
- A transition fires when the source session completes a turn.
- "Completes a turn" means the agent replied and the session is back to `idle`, so it is
  prompt-ready again.
- A transition transforms the completed session's result into a prompt and sends that prompt to
  the destination session.

There is no special orchestrator node in the data model. If the workflow needs a lead,
coordinator, or reviewer session, that is just another regular session on the canvas.

Example graph:

```text
Planner -> Builder -> Reviewer
   ^                    |
   |--------------------|
```

In that graph:

- `Planner` can assign work to `Builder`
- `Builder` can hand implementation results to `Reviewer`
- `Reviewer` can hand review results back to `Planner`

All three are ordinary sessions with ordinary prompts and ordinary histories.

## Template definition

Templates are design-time only. They define reusable session slots, transition rules, and canvas
layout. They do not contain runtime status, live session IDs, or pending work.

```json
{
  "name": "Feature Delivery",
  "description": "Implement, review, and loop until ready.",
  "sessions": [
    {
      "id": "planner",
      "name": "Planner",
      "agent": "Claude",
      "model": "claude-sonnet-4-5",
      "instructions": "Plan the work and decide who should act next.",
      "autoApprove": false,
      "position": { "x": 640, "y": 120 }
    },
    {
      "id": "builder",
      "name": "Builder",
      "agent": "Codex",
      "model": "gpt-5",
      "instructions": "Implement the requested changes.",
      "autoApprove": true,
      "position": { "x": 220, "y": 420 }
    },
    {
      "id": "reviewer",
      "name": "Reviewer",
      "agent": "Claude",
      "instructions": "Review the changes and report issues.",
      "autoApprove": false,
      "position": { "x": 980, "y": 420 }
    }
  ],
  "transitions": [
    {
      "id": "planner-to-builder",
      "fromSessionId": "planner",
      "toSessionId": "builder",
      "trigger": "onCompletion",
      "resultMode": "lastResponse",
      "promptTemplate": "Use this plan and implement it:\n\n{{result}}"
    },
    {
      "id": "builder-to-reviewer",
      "fromSessionId": "builder",
      "toSessionId": "reviewer",
      "trigger": "onCompletion",
      "resultMode": "summaryAndLastResponse",
      "promptTemplate": "Review this implementation:\n\n{{result}}"
    },
    {
      "id": "reviewer-to-planner",
      "fromSessionId": "reviewer",
      "toSessionId": "planner",
      "trigger": "onCompletion",
      "resultMode": "lastResponse",
      "promptTemplate": "Reviewer finished. Decide the next action:\n\n{{result}}"
    }
  ]
}
```

Templates persist to `~/.termal/orchestrators.json`.

## Transition semantics

### Trigger

Phase 1 supports a single trigger:

```rust
enum TransitionTrigger {
    OnCompletion,
}
```

`OnCompletion` means:

- the source session was active
- the agent produced a reply
- the session returned to `idle`
- the session is ready for the next prompt

Transitions do not fire while a session is still active, waiting on approval, or waiting on user
input.

Cyclic graphs are supported. Templates can represent both one-shot flows and intentionally
long-running loops such as planner-reviewer-fixer cycles.

### Result processing

A transition defines how the completed session's output is transformed before it is sent to the
destination.

```rust
enum TransitionResultMode {
    None,
    LastResponse,
    Summary,
    SummaryAndLastResponse,
}
```

- `None`: ignore the source result and rely entirely on `promptTemplate`
- `LastResponse`: use the source session's latest assistant reply
- `Summary`: create a concise summary of the source session
- `SummaryAndLastResponse`: include both

Phase 1 may implement `LastResponse` first and add the summary modes afterward.

### Prompt shaping

Each transition has a `promptTemplate`. The backend renders it using the processed result and then
queues that rendered prompt into the destination session.

At minimum the backend should support:

- `{{result}}`
- `{{sourceSessionId}}`
- `{{sourceSessionName}}`
- `{{transitionId}}`

Example:

```text
Builder finished its turn.

Use this implementation result and decide whether to approve or request changes:

{{result}}
```

## Lifecycle

### 1. Template design

The developer designs a reusable session graph on a canvas:

- add sessions
- set agent/model/instructions per session
- position session cards
- connect sessions with transitions
- configure how each transition turns a completed result into the next prompt

### 2. Instantiation

The developer instantiates a template with a goal or seed prompt.

The backend:

1. creates all sessions from the template
2. records the template snapshot in the orchestration instance
3. optionally sends the initial prompt to one or more starting sessions

Phase 1 can stop at template authoring and management. Runtime orchestration comes after that.

### 3. Runtime loop

At runtime the loop is edge-driven:

```text
Session A becomes prompt-ready after replying
  -> backend identifies matching outgoing transitions
  -> backend builds transition payload(s)
  -> backend renders prompt template(s)
  -> backend queues prompt(s) into destination session(s)
```

If a session has multiple outgoing transitions, multiple destination sessions may be prompted.

## Data model

### Template

```rust
struct OrchestratorTemplate {
    id: String,
    name: String,
    description: String,
    sessions: Vec<OrchestratorSessionTemplate>,
    transitions: Vec<OrchestratorTemplateTransition>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

struct OrchestratorSessionTemplate {
    id: String,
    name: String,
    agent: Agent,
    model: Option<String>,
    instructions: String,
    auto_approve: bool,
    position: CanvasPoint,
}

struct OrchestratorTemplateTransition {
    id: String,
    from_session_id: String,
    to_session_id: String,
    trigger: TransitionTrigger,
    result_mode: TransitionResultMode,
    prompt_template: Option<String>,
}

struct CanvasPoint {
    x: f64,
    y: f64,
}
```

Constraints:

- session IDs are unique within a template
- transition IDs are unique within a template
- `from_session_id` and `to_session_id` must reference existing sessions
- self-loops and larger cycles are allowed

### Runtime instance

This is the shape the runtime should move toward once orchestration execution is implemented:

```rust
struct OrchestratorInstance {
    id: String,
    template_id: String,
    template_snapshot: OrchestratorTemplate,
    status: OrchestratorStatus,
    session_instances: Vec<OrchestratorSessionInstance>,
    pending_transitions: Vec<PendingTransition>,
    created_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
}

struct OrchestratorSessionInstance {
    template_session_id: String,
    session_id: String,
    last_completion_revision: Option<u64>,
    last_delivered_completion_revision: Option<u64>,
}

struct PendingTransition {
    id: String,
    transition_id: String,
    source_session_id: String,
    destination_session_id: String,
    completion_revision: u64,
    rendered_prompt: String,
    created_at: DateTime<Utc>,
}

enum OrchestratorStatus {
    Running,
    Paused,
    Completed,
    Stopped,
}
```

## Backend behavior

### Status hook

The runtime hook is not "generic commit happened." It is the explicit session lifecycle edge:

```text
active -> idle after reply
```

That edge is what creates transition work.

### Delivery model

When a source session completes a turn:

1. find all transitions whose `from_session_id` matches the completed session
2. build the transition result according to `result_mode`
3. render the destination prompt from `prompt_template`
4. persist a `PendingTransition`
5. queue the prompt into the destination session
6. mark that completion revision as delivered

Persisting undelivered transitions is important for restart safety.

### Restart behavior

Running orchestrations survive restart.

On boot:

- reload orchestration instances
- restore session-instance mapping
- restore undelivered `pending_transitions`
- if transitions are still pending, deliver them exactly once
- otherwise resume normal completion watching

This keeps transition delivery deterministic and avoids duplicate prompts after restart.

## API

### Template management

Phase 1 template management API:

```text
GET    /api/orchestrators/templates
POST   /api/orchestrators/templates
GET    /api/orchestrators/templates/{id}
PUT    /api/orchestrators/templates/{id}
DELETE /api/orchestrators/templates/{id}
```

The path can stay `/api/orchestrators/...` even though the template model no longer contains a
special orchestrator node.

### Runtime orchestration

Later phases can add:

```text
GET    /api/orchestrators
POST   /api/orchestrators
GET    /api/orchestrators/{id}
POST   /api/orchestrators/{id}/pause
POST   /api/orchestrators/{id}/resume
POST   /api/orchestrators/{id}/stop
```

## UI

### Template library

The control panel and settings should let the developer:

- browse saved templates
- start a blank orchestration canvas
- open an existing template into the editor
- create, update, and delete templates

### Canvas editor

The canvas is a graph editor:

- each card is a regular session template
- edges represent transitions
- dragging cards changes only layout
- editing a transition changes trigger/result-mode/prompt-template

There is no visually special orchestrator card by default. If the developer wants a central
coordinator session, they create one explicitly and place it where they want on the canvas.

## Design decisions

### D1: No special orchestrator node

Resolved. The orchestration graph contains only regular sessions. A "main" or "planner" role is
just another session in `sessions`.

### D2: Transition trigger meaning

Resolved. `OnCompletion` means the source agent replied and the session became prompt-ready again.

### D3: Transition ownership

Resolved. A transition belongs to the edge, not the node. It defines how a completed result is
processed and how the destination prompt is built.

### D4: Session creation

Resolved. Transitions do not create sessions. They route work only between sessions that already
exist in the orchestration instance.

### D5: Summary generation

Resolved. Summary-based result modes use a fresh summarizer session if and when those modes are
implemented. The summary is not generated by asking the source session to summarize itself.

## Implementation phases

### Phase 1: Template design and management

- Define the session-only template schema
- Persist templates to `~/.termal/orchestrators.json`
- Add template CRUD API
- Add template library UI
- Add blank-canvas and edit-canvas flows
- Support sessions + transitions + canvas layout in the editor

### Phase 2: Runtime graph instantiation

- Instantiate all template sessions
- Track orchestration instances and session-instance mapping
- Add launch flow and instance state

### Phase 3: Transition execution

- Detect `OnCompletion` edges from session lifecycle
- Build transition outputs
- Persist and deliver `pending_transitions`
- Queue prompts into destination sessions
- Add restart-safe exactly-once transition delivery

### Phase 4: Summary modes and polish

- Implement `Summary` and `SummaryAndLastResponse`
- Add runtime canvas state and transition activity indicators
- Add pause/resume/stop

## Testing plan

**Backend**

- template CRUD
- validation of session IDs and transition IDs
- rejection of transitions that reference unknown sessions
- acceptance of self-loop and multi-session cyclic transitions
- transition delivery exactly once for a given completion revision
- restart recovery for undelivered transitions

**Frontend**

- create/edit/delete templates
- blank canvas launch
- open existing template into canvas editor
- add/remove sessions
- add/remove transitions
- drag canvas cards
- validate graph errors in the editor

**Integration**

- instantiate a graph and create all declared sessions
- complete one session turn and verify the matching destination prompt is enqueued
- complete a source with multiple outgoing edges and verify multiple prompts are enqueued
- restart with undelivered transitions and verify they are delivered once

## Acceptance criteria

- A developer can design and persist a template made only of sessions and transitions.
- A developer can open a blank canvas or edit an existing template from the UI.
- The template editor does not require a special orchestrator node.
- Transitions are defined as connections between sessions.
- The transition trigger is defined as a source session becoming prompt-ready after replying.
- The runtime design supports restart-safe delivery of transition-generated prompts.
