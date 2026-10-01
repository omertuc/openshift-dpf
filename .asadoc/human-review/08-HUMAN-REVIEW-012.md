# HUMAN-REVIEW-012: `openshift-docs:nw-dpf-creating-dpfhcpprovisioner/yaml-001`

- **Priority:** 8
- **Status:** matched (hides drift, code changed)
- **TODO:** TODO on the marker in `manifests/dpf-hcp-provisioner-operator/dpfhcpprovisioner-cr-template.yaml`

Options drop `machineOSURL` (the doc says the operator derives that image from `blueFieldOCPLayerRepo`) and `controlPlaneAvailabilityPolicy` (optional in the doc, set from `VM_COUNT` in the repo).

The agent also changed `scripts/dpf.sh`: `virtualIP` used to be appended with a heredoc only when `HYPERSHIFT_API_IP` is set. Now the template always has a `<HYPERSHIFT_API_IP>` line, and `sed` deletes it when the variable is unset. Tested only by simulation.

**Decide:** whether `machineOSURL` is drift, and whether the `virtualIP` rewrite is acceptable.
