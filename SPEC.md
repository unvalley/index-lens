# Elasticsearch TUI - Minimal Spec

## Goal
Create a minimal terminal UI for Elasticsearch using ratatui that shows basic cluster health and allows manual refresh.

## Scope (v0.1)
- Terminal UI with header, main panel, footer.
- Fetch `/_cluster/health` from an Elasticsearch endpoint.
- Show key fields: `cluster_name`, `status`, `number_of_nodes`, `active_primary_shards`, `active_shards`, `unassigned_shards`.
- Fetch indices from `/_cat/indices?format=json`.
- Show indices list and allow selection with ↑/↓.
- Fetch paged documents for selected index via `/{index}/_search?from=&size=`.
- Show index details (shards/replicas/uuid) for selected index.
- Interactive document list with selection and preview.
- Filter documents with a query-string input (`/` to edit).
- Manual refresh with `r`, quit with `q`, load docs with `d`/Enter, page with `n/p`.
- Auto refresh every 10 seconds.
- Show last refresh age and any last error.

## Assumptions
- Elasticsearch is reachable via HTTP.
- Default URL: `http://localhost:9200`.
- Override via `ES_URL` environment variable.

## Non-goals (v0.1)
- Authentication, TLS, or API key support.
- Complex query execution or pagination.
- Persistent config storage.

## Success Criteria
- App starts in alternate screen, draws UI, and does not crash on missing ES.
- Manual refresh works and errors are shown in the UI.
- Quitting returns terminal to a normal state.
