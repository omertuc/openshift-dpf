# HUMAN-REVIEW-027: `openshift-docs:nw-dpf-creating-hcp-secrets/terminal-002`

- **Priority:** 4
- **Status:** matched (code changed)
- **TODO:** TODO on the marker in `scripts/dpf.sh` (section "hcp-pull-secret")

Flags were reordered (`--type=Opaque` before `-n`) and an option strips the repo's `|| true`. Both the doc and the repo use `--type=Opaque` for a `.dockerconfigjson` secret, which is unusual.

**Decide:** whether `--type=Opaque` is right for a pull secret.
