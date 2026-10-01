# HUMAN-REVIEW-024: `openshift-docs:nw-dpf-creating-sriov-config/yaml-001`

- **Priority:** 5
- **Status:** matched (hides drift)
- **TODO:** TODO on the marker in `manifests/post-installation/nodesriovdevicepluginconfig.yaml`

The repo's SR-IOV config has a Kata VF pool line that the doc doesn't; a wildcard option removes it. The pool names come from params.

**Decide:** whether the Kata pool is an intended repo-only difference.
