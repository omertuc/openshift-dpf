# HUMAN-REVIEW-032: `openshift-docs:nw-dpf-installing-mce-operator/terminal-003`

- **Priority:** 4
- **Status:** ignored (manual-command)
- **TODO:** `# TODO` comment in `scripts/tools.sh` (no marker for this block)

The doc waits for the MCE CRD with `oc wait crd` (created and Established). The repo uses retry loops for the CSV, the CRD and the webhook endpoints instead.

**Decide:** whether ignoring is right.
