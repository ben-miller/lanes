# Observation/caching daemon: API layer tradeoffs (GraphQL vs. alternatives)

Status: design notes from a discussion. Not a decision, not a spec. Holding
off on committing to an approach until there's more real signal to design
against.

## Context

The eventual daemon that maintains live observations (see the
`scope::ScopeElement`/`Observation`/`signals_from()` work already built) will
need some kind of API surface for consumers (the Lanes Switch UI today,
possibly agents or other consumers later) to read current state and receive
updates as it changes. This doc captures a discussion about whether that API
should be GraphQL, and what it should transport over.

Infrastructure choice already made independently of this question: the
daemon will be tokio + HTTP, loosely inspired by Zellij's client-server
architecture (a persistent background process, thin clients talking to it).

## What GraphQL actually offers here

- **Resolver-per-type + field selection**: clients ask for exactly the
  fields they want per object type; the server only computes what's asked
  for. Real value once there are multiple consumers wanting different
  shapes/depths of the same underlying data.
- **Subscriptions**: a native, spec'd mechanism for server-push, not bolted
  on. Can run over `graphql-ws` (WebSocket, most common) or `graphql-sse`
  (SSE, a real alternative) - not exclusively WebSocket-bound.
- **Important caveat**: subscriptions push full resolved payloads per event,
  not diffs. There's no built-in "here's revision N, here's the diff to
  N+1" semantics - GraphQL doesn't track prior state between events. Any
  snapshot-then-delta protocol (the kind sketched in earlier, informal
  design notes) would have to be hand-built on top of subscriptions, same
  amount of work as building it on raw SSE directly. GraphQL gives you the
  query/field-selection layer and a standard push transport, not delta
  semantics.

## What it costs

- Originally flagged as "a big async server stack you don't have" - but
  since tokio + HTTP are already planned regardless of this decision, that
  objection mostly evaporates. What GraphQL adds *on top of* an HTTP server
  you're already building is smaller: schema definition, resolver dispatch,
  query parsing/validation. Real, but marginal once the base infra is a
  given.
- Real cost that remains: **schema-as-contract, decided before the real
  shape is known.** With 3 provider kinds and effectively one consumer
  today, a GraphQL schema (or any formal API schema) risks locking in
  guesses rather than observed usage. Schemas are more expensive to change
  once something depends on them than an unstructured JSON API is.
- Client-side cost isn't confined to the Rust server: the Svelte UI
  currently just calls `invoke()` with no query layer; a Hammerspoon/Lua
  consumer would need an HTTP client regardless of GraphQL vs. plain JSON,
  but GraphQL specifically would want query construction/codegen on every
  client, not just the server.

## Why not just copy Zellij's transport (Unix socket + custom binary protocol)

Verified via search: Zellij's client-server IPC is Unix domain sockets with
a protobuf-serialized custom protocol, not HTTP
([source](https://deepwiki.com/zellij-org/zellij/2.1-client-server-model)).
That choice fits Zellij's actual workload: local-only, latency-sensitive,
high-frequency small messages (every keystroke, every screen redraw) - HTTP
overhead would matter a lot there for zero benefit, since remote access was
never the point.

Lanes' workload is the opposite shape on the dimension that drove that
choice: observations change occasionally (a repo goes dirty, a session
finishes), not at terminal-frame-rate, and being curl-able / reachable from
any language's standard HTTP client (including Lua/Hammerspoon, which would
struggle with a custom binary protocol) is worth more here than the
last-mile latency savings of a raw socket. So "persistent background
process, thin clients" is the part worth borrowing from Zellij; the
transport choice (HTTP vs. Unix socket) is a separate decision that doesn't
follow from the Zellij comparison, and HTTP is the more defensible pick for
*this* project's actual needs even though it's not what Zellij itself does.

## Prior art

**Home Assistant** - closest architectural analog: a local hub aggregating
state from a large number of heterogeneous entities (lights, sensors,
hundreds of integrations), serving a UI that reacts to changes. Chose
**REST + WebSocket, not GraphQL**, and is moving *away* from REST toward
WebSocket as primary - specifically because what clients need is real-time
push across many flat entity types, not flexible ad hoc querying. Even at
much larger scale than Lanes' current 3 kinds, query-shape flexibility
wasn't the bottleneck; push was, and a simpler typed-WebSocket-message
mechanism served that without schema/query-engine overhead.
([source](https://deepwiki.com/home-assistant/developers.home-assistant/6.2-rest-and-websocket-apis))

**Backstage** (Spotify's developer portal - catalogs services, ownership,
APIs, docs) - opposite lesson. **Started with plain REST**, one API per
backend plugin. GraphQL was added *later*, as an ecosystem plugin (Roadie's
GraphQL Catalog), specifically because plugin authors were "juggling REST
endpoints... half-documented," and because the catalog has **rich relations**
between entity types (services, owners, groups, APIs, dependencies) that
consumers wanted to traverse in one request. Classic GraphQL win: not just
many types, but many types with graph-shaped relations, multiple consumers
wanting different traversal depths - and it was layered on top of a simpler
existing API once that pain was real, not designed in from day one.
([source](https://roadie.io/backstage/plugins/graph-ql-catalog/))

## Where this leaves things

A heuristic, not a decision: GraphQL earns its place once there are (a)
genuinely many provider/entity kinds, (b) real relations between them worth
traversing (not just a flat list of unrelated things), and (c) multiple
independently-evolving consumers wanting different field-level shapes. Until
then, formalizing *any* query API - GraphQL or hand-rolled REST/JSON alike -
risks locking in a shape based on guesses. The caution isn't GraphQL-specific.

Today, Lanes has 3 flat, unrelated scope element kinds and effectively one
consumer (`gather_lanes()` → the UI) - closer to Home Assistant's shape
(many flat entities, need push) than Backstage's (few types, deep relations,
many consumers). If the model grows toward real relations between kinds
(a Claude session belonging to a repo belonging to a Jira project with
issues assigned across people) and a second real consumer shows up wanting
a different slice of that, that's the point this is worth revisiting for
real - and given tokio + HTTP will already exist by then, adding GraphQL on
top at that point is a much smaller lift than deciding it now.
