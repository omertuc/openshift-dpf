# HUMAN-REVIEW-003: `openshift-docs:nw-dpf-installing-grafana-for-dts/terminal-001`

- **Priority:** 9
- **Status:** unresolved
- **TODO:** `# TODO` comment in `manifests/observability/operators/grafana-operator-subscription.yaml` (no marker for this block)

The doc installs grafana-operator with Helm (chart 5.24.0, namespace `grafana-operator`). The repo installs it through an OLM Subscription (channel v5, `openshift-operators`).

**Decide:** switch the repo to Helm, or ignore this block. HUMAN-REVIEW-004 and HUMAN-REVIEW-030 depend on the same decision.
