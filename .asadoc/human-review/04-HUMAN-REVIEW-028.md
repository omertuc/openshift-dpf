# HUMAN-REVIEW-028: `openshift-docs:nw-dpf-configuring-hosted-cluster-auth/terminal-001`

- **Priority:** 4
- **Status:** ignored (manual-command)
- **TODO:** `# TODO` comment in `scripts/utils.sh` (no marker for this block)

`ensure_hosted_kubeconfig` does the same thing as the doc's command differently (temp file, different argument order), so it can't be matched without changing behaviour.

**Decide:** whether ignoring is right.
