# HUMAN-REVIEW-014: `openshift-docs:nw-dpf-adding-workers-baremetal-operator/yaml-002`

- **Priority:** 7
- **Status:** unresolved
- **TODO:** `# TODO` comment in `manifests/worker-provisioning/bmc-secret.yaml` (no marker for this block)

The doc's BMC secret uses `stringData` with a plain username and password. The repo's `bmc-secret.yaml` uses `data` with base64 values. Switching the repo would put raw passwords into YAML through `sed`, which breaks on special characters.

**Decide:** likely a doc-side question; which form should the doc show?
