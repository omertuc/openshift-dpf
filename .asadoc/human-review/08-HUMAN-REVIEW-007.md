# HUMAN-REVIEW-007: `openshift-docs:nw-dpf-installing-grafana-for-dts/yaml-003`

- **Priority:** 8
- **Status:** unresolved (closest: `manifests/observability/grafana/grafana-cr.yaml`, 13 of 37 lines differ)
- **TODO:** TODO on the marker in `manifests/observability/grafana/grafana-cr.yaml`

The doc's Grafana CR adds a `deployment.spec` nodeSelector and tolerations that pin Grafana to control-plane nodes. The repo's `grafana-cr.yaml` has neither.

**Decide:** add them to the repo, or drop them from the doc.
