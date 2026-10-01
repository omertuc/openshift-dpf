# HUMAN-REVIEW-008: `openshift-docs:nw-dpf-installing-maintenance-operator/yaml-001`

- **Priority:** 8
- **Status:** matched (hides drift)
- **TODO:** TODO on the marker in `manifests/helm-charts-values/maintenance-operator-values.yaml` (section "maintenance-operator-values")

The doc's values leave out `operatorConfig.deploy: true`, which the repo sets. In maintenance-operator-chart 0.3.0 `deploy` defaults to `false`, so following the doc creates no MaintenanceOperatorConfig and `maxParallelOperations: 60%` has no effect. That looks like a doc bug.

The marker hides the line with `remove-lines-starting-with: "deploy: true"`. Earlier test runs left this block unresolved instead.

**Decide:** likely remove that option so the block fails until the doc is fixed.
