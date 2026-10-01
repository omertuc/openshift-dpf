# HUMAN-REVIEW-017: `openshift-docs:nw-dpf-installing-gitops-operator/yaml-001`

- **Priority:** 6
- **Status:** matched (hides drift)
- **TODO:** TODO on the marker in `manifests/gitops-operator/subscription.yaml`

The subscription was reformatted (keys alphabetical, blank lines and leading `---` removed). The repo pins `startingCSV` to `GITOPS_OPERATOR_VERSION` and the doc doesn't; `remove-lines-starting-with: "startingCSV:"` hides it.

**Decide:** whether the `startingCSV` pin is an intended repo-only difference.
