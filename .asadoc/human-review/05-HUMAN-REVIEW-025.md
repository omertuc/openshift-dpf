# HUMAN-REVIEW-025: `openshift-docs:nw-dpf-creating-service-ipam/yaml-001`

- **Priority:** 5
- **Status:** matched (code changed)
- **TODO:** TODO on the marker in `manifests/post-installation/dpuservice-ipam.yaml`

The doc shows both IPAM objects in one block, so the agent merged `hbn-ovn-ipam.yaml` and `hbn-loopback-ipam.yaml` into the new `dpuservice-ipam.yaml` and updated `scripts/post-install.sh` to use it.

**Decide:** whether the merge is acceptable.
