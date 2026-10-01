# HUMAN-REVIEW-006: `openshift-docs:nw-dpf-enabling-ovnk-resource-injector/terminal-001`

- **Priority:** 9
- **Status:** matched (hides drift)
- **TODO:** TODO on the marker in `scripts/enable-ovn-injector.sh` (section "ovn-injector-helm-install")

Matches only because a doc-side option drops the doc's `--skip-crds`, which the repo doesn't pass. The repo also passes `--take-ownership`, which the doc doesn't have.

The agent also restructured the script: the `helm_args=(...)` array became a direct `helm upgrade` call, with the Kata flags in a separate `kata_args` array. The helm arguments should be the same as before.

**Decide:** whether `--skip-crds` / `--take-ownership` are real drift (then leave the block unresolved instead of hiding it), and whether the restructure is acceptable.
