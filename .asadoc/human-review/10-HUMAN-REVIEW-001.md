# HUMAN-REVIEW-001: `openshift-docs:nw-dpf-installing-cert-manager-operator/yaml-001`

- **Priority:** 10
- **Status:** unresolved
- **TODO:** `# TODO` comment in `manifests/cluster-installation/openshift-cert-manager.yaml` (no marker for this block)

The doc and the repo install cert-manager differently:

| | Doc | Repo (`openshift-cert-manager.yaml`) |
|---|---|---|
| Namespace | `cert-manager` | `cert-manager-operator` |
| OperatorGroup | `openshift-cert-manager-operator` | `cert-manager-operator-group` |
| Subscription namespace | `cert-manager` | `openshift-operators` (not its own OperatorGroup's namespace) |

**Decide:** which side is right. Matching would change what the repo installs, so it was left failing.
