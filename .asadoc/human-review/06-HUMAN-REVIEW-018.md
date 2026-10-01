# HUMAN-REVIEW-018: `openshift-docs:nw-dpf-creating-ovnk-service/yaml-001`

- **Priority:** 6
- **Status:** matched (hides drift)
- **TODO:** TODO on the marker in `manifests/post-installation/ovn-configuration.yaml`

The repo's OVN DPUServiceConfiguration sets `global.imagePullSecretName`; the doc doesn't. An option removes the line. Quotes were also removed from placeholders.

**Decide:** whether the doc is missing the pull secret.
