# HUMAN-REVIEW-016: `openshift-docs:nw-dpf-configuring-required-operators/terminal-012`

- **Priority:** 6
- **Status:** ignored (manual-command)
- **TODO:** `# TODO` comment in `scripts/tools.sh` (no marker for this block)

The doc enables hypershift in the MultiClusterEngine with a JSON-patch `add`. The repo's equivalent in `install_hypershift_via_mce` uses a merge patch, which replaces the whole `spec.overrides.components` list and could wipe other overrides.

**Decide:** whether that's a repo bug worth fixing.
