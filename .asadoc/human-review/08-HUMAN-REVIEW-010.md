# HUMAN-REVIEW-010: `openshift-docs:nw-dpf-installing-metallb-operator/yaml-001`

- **Priority:** 8
- **Status:** matched (hides drift)
- **TODO:** TODO on the marker in `manifests/metallb/metallb-subscription.yaml`

The repo creates an OperatorGroup in `metallb-system` that the doc doesn't have. It looks unused, since the Subscription is in `openshift-operators`. A `remove-text` regex hides it.

**Decide:** drop the OperatorGroup from the repo, or add it to the doc.
