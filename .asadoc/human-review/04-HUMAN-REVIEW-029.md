# HUMAN-REVIEW-029: `openshift-docs:nw-dpf-adding-workers-baremetal-operator/yaml-004`

- **Priority:** 4
- **Status:** matched (code changed)
- **TODO:** TODO on the marker in `manifests/worker-provisioning/baremetalhost.yaml`

`rootDeviceHints` was moved up to match the doc's key order. `userData` stays last, because `maybe_attach_nno_jumbo_mtu` appends a spec key to the end of the file.

**Decide:** check nothing else depends on the old key order.
