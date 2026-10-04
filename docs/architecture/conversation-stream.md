# Conversation stream contract

The conversation UI is a projection pipeline, not an independent source of
session truth:

```text
EventLog / live EventPayload
  → server conversation projection
  → ConversationStreamEnvelopeDto
  → UI delta buffer
  → pure conversation reducer
  → render model
```

## Ownership boundaries

- `astrcode-core::EventPayload` contains runtime and durable domain facts.
- `astrcode-server::http::projection` maps those facts at the HTTP boundary.
- `astrcode-protocol` owns snapshot, block, delta, cursor, and envelope wire
  contracts.
- `astrcode-ui::conversation` owns the delta buffer, order-preserving
  coalescing, and pure state reduction. It does not depend on gpui, so it is
  testable without a window or a GPU.
- `astrcode-gui` and `astrcode-webui` are the two hosts of that shared layer.
  Both render already-projected conversation blocks and do not reconstruct
  backend state.

The shared fixtures in `crates/astrcode-protocol/fixtures` are consumed by the
Rust contract tests in `crates/astrcode-protocol/src/http/tests.rs`. Any wire
change must update the fixture and the contract test together.

## Snapshot and cursor invariants

1. A snapshot is the complete render state at its cursor.
2. Every stream envelope carries the latest parent-session durable cursor known
   when the delta is emitted.
3. Agent-child token, reasoning, and tool-output deltas remain scoped to the
   child fan-out. Only phase-boundary signals and compact lineage changes enter
   the parent conversation and update its child-agent projection.
4. A child cursor must never replace the parent cursor.
5. Reconnect replays durable events strictly after the supplied cursor.
6. Invalid, ahead-of-head, or over-limit cursors produce `rehydrateRequired`.
   The old stream closes after that marker so it cannot race the replacement
   snapshot.
7. Once a stream has opened, any unexpected end or read error refreshes the
   snapshot before reconnecting because live-only fragments are not replayable.
8. Applying replay followed by live deltas must converge to the same visible
   state as fetching a fresh snapshot.

## Delta buffering

Deltas are accumulated and their memory bounded by a delta-count and text-size
budget. The buffer:

- keeps the newest cursor for the batch it hands over;
- flushes without dropping data when either budget is reached;
- hands the drained batch to the reducer, which is the single owner of
  order-preserving delta coalescing.

There is no per-animation-frame flush. gpui already provides an entity
notification cadence, so the shared layer keeps only the buffer and its memory
bound and lets the host drive redraws (ADR 0001, round 13).

The size limit is a memory bound, not backpressure sent to the server.

## Rendering policy

- User and assistant bodies render through per-block Markdown state keyed by
  block id. A block's state lives exactly as long as the block does.
- Appending to a block degrades to an append in the Markdown state rather than
  re-parsing the whole document, so streaming does not cost the full body per
  delta.
- Tool details stay hidden until expanded, and collapsed regions are forced
  open while they hold a pending decision.
- Per-frame render cost scales with the bytes rendered, not with the held
  history. The measured budgets, and the three conditions that make streaming
  redraws viable, are recorded in [ADR 0001](adr/0001-replace-web-frontend-with-gpui-kit.md)
  under the cadence spike conclusion.

## Change checklist

When changing conversation events or rendering:

1. Decide whether the value is a durable fact, a live-only hint, or a wire-only
   projection.
2. Keep DTO mapping in the server/protocol boundary.
3. Update live projection, replay projection, and snapshot projection together.
4. Extend the shared reducer fixture.
5. Verify reconnect, rehydrate, compact continuation, and child-session routing.
6. Check whether the change moves per-frame render cost, and compare against the
   budget recorded in the ADR cadence spike conclusion.
