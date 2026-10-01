# HUMAN-REVIEW-013: `openshift-docs:nw-dpf-creating-dpfoperatorconfig/yaml-001`

- **Priority:** 8
- **Status:** matched (code changed)
- **TODO:** TODO on the marker in `manifests/dpf-installation/dpfoperatorconfig.yaml`

The marker with the most options. The template and `scripts/manifests.sh` were rewritten: keys follow the doc's order, the doc's inline comments are in the template, and the `<FLANNEL_CONFIG>` placeholder became a real `flannel: podCIDR: <FLANNEL_POD_CIDR>` block that `manifests.sh` deletes for OCP < 4.22 (instead of inserting it for 4.22+). The agent simulated rendering on 4.21 and 4.22 and got the same output.

TODOs on the marker flag possible drift: the doc leaves out `imagePullSecrets`, and it takes the API server port from `$TARGETCLUSTER_API_SERVER_PORT` while the repo hard-codes 6443.

**Decide:** whether the template/script rewrite is acceptable, and whether the TODO'd differences are drift.
