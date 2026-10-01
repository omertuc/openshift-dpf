# HUMAN-REVIEW-002: `openshift-docs:nw-dpf-installing-dpf-operator/terminal-003`

- **Priority:** 9
- **Status:** unresolved
- **TODO:** `# TODO` comment in `scripts/dpf.sh` (no marker for this block)

The doc installs the DPF operator chart with `--set` flags and `--wait`. The repo uses a values file (which also sets `imagePullSecrets`), adds `--create-namespace` and `--disable-openapi-validation`, and doesn't pass `--wait`.

**Decide:** whether the repo or the doc should change. Matching would change behaviour, so it was left failing.
