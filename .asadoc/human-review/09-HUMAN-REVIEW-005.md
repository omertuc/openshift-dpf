# HUMAN-REVIEW-005: `openshift-docs:nw-dpf-deploying-traffic-test-pods/yaml-001`

- **Priority:** 9
- **Status:** unresolved (closest: `manifests/testing/workload.yaml`, 2 lines differ)
- **TODO:** TODO on the marker in `manifests/testing/workload.yaml`

The doc sets `replicas: 1` on both worker Deployments, but its own sample output shows two worker pods. The repo uses `replicas: 2` (the e2e test rewrites it to the node count).

The marker also uses params for the `sriov-test-*` vs the doc's `traffic-test-*` names and for the quay mirror image. Renaming in the repo would touch `test/e2e`, `test/manifests` and `scripts/dpf-sanity-checks.sh`.

**Decide:** most likely a doc fix (`replicas: 2`). Also whether the name and image params are acceptable.
