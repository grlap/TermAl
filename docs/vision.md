# TermAl Vision Draft

This is a product vision draft, not a delivery checklist.

For phased rollout, see [`roadmap.md`](./roadmap.md).
For implementation backlog, see [`bugs.md`](./bugs.md).

## Framing

TermAl started from the terminal because that was the fastest practical entry point into real AI
coding workflows.

That does not mean the product should stay bounded by terminal assumptions.

The terminal was the entry point. It is not the product boundary.

## Vision

TermAl is an operating environment for coding agents.

It should give a developer one place to run, supervise, review, steer, and eventually collaborate
around long-running software work performed by AI agents.

The product should feel purpose-built for agent-driven engineering work:

- sessions instead of disposable one-shot prompts
- structured outputs instead of raw terminal noise
- explicit approvals instead of hidden side effects
- diff and review workflows instead of blind trust
- audit and guardrails instead of ad hoc caution
- remote supervision and collaboration instead of machine-local isolation

## Core belief

Working effectively with coding agents requires new product primitives.

Generic terminal and chat interfaces are useful starting points, but they are not sufficient on
their own for the workflows that matter most:

- long-running tasks
- multi-step tool use
- file change review
- approvals for risky actions
- queued follow-ups
- reproducibility and auditability
- remote supervision
- collaborative review and pair programming

TermAl should be designed around those primitives directly.

## Humans + Agents

The right mental model is not "AI replaces software engineering."

It is "humans and agents work together inside a system that protects correctness."

Agents expand execution bandwidth. Humans hold intent, judgment, and responsibility for whether the
work is actually correct.

That makes correctness the real bottleneck, not code generation speed.

## Correctness and SDLC

Agentic development can make a strong senior engineer dramatically more productive.

That leverage is only real if the surrounding SDLC is strong enough to keep the work correct.

In an agent-heavy environment, SDLC is not secondary process overhead. It becomes the control
system for accelerated engineering work.

That includes:

- clear task framing
- reviewable diffs
- tests and verification
- approvals for risky actions
- checkpoints and rollback
- audit history
- explicit unresolved feedback

Without those controls, agent speed just increases the rate at which mistakes spread.

## Product thesis

If coding agents become part of normal engineering work, developers will need something closer to a
control room than a terminal window.

That control room should let people:

- run multiple agent sessions at once
- understand what each agent is doing
- interrupt, redirect, or approve work at the right moments
- inspect changes before accepting them
- leave feedback in the context of the changes
- return later without losing the thread
- access the same work remotely
- eventually collaborate with another human around the same agent session

## Pair programming in the agent era

Pair programming becomes more important, not less, when agents increase individual throughput.

One senior developer with strong agent workflows can otherwise occupy too much project space too
quickly. The codebase changes faster than peers can track, review, or meaningfully shape.

Pair programming helps close that gap by keeping two humans inside the same fast-moving context.

This is cooperative overlap:

- shared control of the same changing system
- shared judgment on correctness and tradeoffs
- shared understanding of why changes are being made
- shared responsibility for what gets accepted

In that sense, pairing is not just a coding style. It is a synchronization mechanism for
high-velocity agent-assisted development.

It can also help narrow the gap between senior and junior engineers if the workflow keeps the
junior inside the reasoning and review loop instead of turning them into a passive consumer of
generated code.

## What TermAl should be

- An agent-native workspace for coding tasks
- A structured review and approval surface
- A durable system of record for agent work
- A remote supervision tool for machines running agent sessions
- A future collaboration layer for human-plus-agent pair programming

## What TermAl should not be

- Just a prettier terminal emulator
- Just a chat wrapper around CLI tools
- Just an IDE clone with AI features bolted on
- Just a remote desktop for watching an agent type
- Just a generic collaboration app

## Guiding principles

### 1. Agent-native first

The UI should model real agent actions directly: prompts, tool calls, commands, diffs, approvals,
review comments, replies, and queued work.

### 2. Trust through structure

The product should reduce ambiguity.

Users should be able to see:

- what the agent did
- what changed
- what was approved
- what is still pending
- what feedback remains unresolved

Correctness needs to stay legible even when output speed increases.

### 3. Durable context

Agent work is often multi-turn and multi-hour. Sessions, review comments, audit history, and queued
work should survive interruption and restart.

### 4. Remote by design

Even when the first product is local-only, the core models should survive remote access later.

Messages, sessions, approvals, reviews, and audit trails should not depend on being on the same
machine as the UI.

### 5. Collaboration is downstream of clarity

Remote pair programming and multi-user workflows only work if the single-user product is already
clear, trustworthy, and structured.

### 6. Acceleration must stay collaborative

The system should help high-output engineers stay aligned with peers instead of becoming isolated
through speed.

Shared review, shared session context, shared audit, and pair-oriented workflows are part of the
product, not extra polish.

## North-star experience

A developer can leave a machine running agent sessions, open TermAl later from another device,
review diffs, leave comments, ask for follow-up work, approve safe actions, reject risky ones, and
bring in another engineer when collaborative judgment is needed.

The entire flow should feel like operating an intelligent engineering workspace, not wrestling with
a terminal transcript.

## Short version

TermAl began at the terminal, but its real destination is broader:

an operating environment for software engineering work done with AI agents.

