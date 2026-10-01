# HUMAN-REVIEW-022: `openshift-docs:nw-dpf-worker-machineconfig/terminal-002`

- **Priority:** 5
- **Status:** matched (code changed)
- **TODO:** TODO on the marker in `scripts/dpf.sh` (section "dpu-worker-config-install")

The helm flags were reordered to the doc's order. The marker uses `remove-prefix: "if "` / `remove-suffix: "; then"`, a quoted chart URL param, and the broad `${**}` param for `${version_flag}`.

**Decide:** whether the options and the broad param are acceptable.
