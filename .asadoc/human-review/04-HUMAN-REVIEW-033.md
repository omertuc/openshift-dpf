# HUMAN-REVIEW-033: `openshift-docs:nw-dpf-creating-dpuflavor/yaml-001`

- **Priority:** 4
- **Status:** matched
- **TODO:** TODO on the marker in `manifests/post-installation/dpuflavor-1500.yaml` (section "dpuflavor-1500")

Marked as a section so the commented-out TODO config at the end of the file stays outside it. The section's end marker sits right after a YAML block scalar; the parsed YAML is unchanged.

**Decide:** check the end marker placement is safe.
