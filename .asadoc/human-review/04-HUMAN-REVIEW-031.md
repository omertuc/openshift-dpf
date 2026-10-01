# HUMAN-REVIEW-031: `openshift-docs:nw-dpf-viewing-dts-metrics/text-001`

- **Priority:** 4
- **Status:** ignored (no-repo-source)
- **TODO:** TODO on the marker in `manifests/observability/grafana/dts-grafana-dashboard.yaml`

The doc's PromQL (`current_link_speed{job=~"doca-telemetry-service.*"}`) isn't used as-is in the repo's dashboards, which filter on `source` instead.

**Decide:** whether the doc's query works with the repo's setup.
