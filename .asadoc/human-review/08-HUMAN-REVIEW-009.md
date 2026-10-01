# HUMAN-REVIEW-009: `openshift-docs:nw-dpf-configuring-required-operators/yaml-001`

- **Priority:** 8
- **Status:** matched (hides drift, doc-side option)
- **TODO:** TODO on the marker in `manifests/dpf-installation/nfd-cr-template.yaml` (section "nfd-instance")

The doc names the NodeFeatureDiscovery CR `nfd-instance`; the repo names it `nfd`. A doc-side `remove-text` hides the difference. Renaming it in the repo would leave a second NFD CR on existing clusters.

**Decide:** whether the name difference matters, and if so, leave the block unresolved instead of hiding it.
