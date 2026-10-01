# HUMAN-REVIEW-011: `openshift-docs:nw-dpf-installing-hcp-provisioner/terminal-002`

- **Priority:** 8
- **Status:** matched (hides drift)
- **TODO:** TODO on the marker in `scripts/dpf.sh` (section "dpf-hcp-provisioner-operator-install")

The repo installs a dev chart from quay, overrides the operator image (`latest`, `pullPolicy: Always`) and passes `--disable-openapi-validation`; the doc has none of these. The marker covers them with options and a broad `param: "${**}"` (needed only for `${version_flag}`).

**Decide:** whether these are intended repo-only differences (keep, with the TODOs) or drift (leave unresolved).
