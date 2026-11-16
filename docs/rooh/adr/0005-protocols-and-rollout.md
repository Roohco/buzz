# ADR 0005: Pilot transport first; add protocols for proven consumers

Status: proposed technical decision, 2026-11-16.

## Context

The goal spans interactive agent sessions, tool access, cross-project work and eventual external interoperability. ACP, MCP, A2A and Buzz job kinds solve different parts of that problem. Introducing all of them into the first internal pilot would create overlapping task semantics before there is a real user path.

## Decision

Use existing Buzz signed channel messages and broker read/post/reply for the real-relay proof and first project result projections. Keep ACP for interactive owner-agent sessions and MCP for individually authorized tool calls. Use the existing Figma2Code/Strands graph inside that harness. Add Buzz typed jobs only after publisher, consumer, cancel, replay and permission contracts are verified. Add A2A only for a named external agent service; do not create a parallel internal job bus for two Rooh adapters.

Roll out in order: real-relay broker proof → QA completion projection → one-screen Figma candidate → typed jobs and authorized peer delegation → durable approvals and guarded Git effects → voice interface over the same grants. Each stage has a separate [DoD](../DOD.md). Voice is an input mode, never an authority source.

## Consequences

- The initial pilot can validate reviewer value without a new protocol implementation.
- Task state is limited until typed jobs ship; present the limit honestly.
- Cross-agent autonomy is staged behind explicit grants and bounded handoffs.
- External interoperability remains possible without forcing A2A into internal control flow.

## Alternatives considered

- A2A everywhere: unnecessary second task model for current internal harnesses.
- Global Strands Swarm: attractive autonomy demo but unbounded handoff and unclear authority across projects.
- New HTTP task API: duplicates Buzz event transport and its membership gate.

## References

- [ACP protocol](https://github.com/agentclientprotocol/agent-client-protocol/blob/main/docs/protocol/v2/overview.mdx)
- [MCP authorization](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2026-07-28/basic/authorization/index.mdx)
- [A2A Agent Card](https://a2a-protocol.org/dev/specification/agent-card/)
- [Strands multi-agent patterns](https://strandsagents.com/docs/user-guide/sdk/multi-agent/multi-agent-patterns/)
