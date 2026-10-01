# HUMAN-REVIEW-015: `openshift-docs:nw-dpf-management-cluster-setup/yaml-001`

- **Priority:** 6
- **Status:** ignored (no-repo-source)
- **TODO:** `# TODO` comment in `scripts/cluster.sh` (no marker for this block)

The nmstate template for jumbo MTU. Its closest code is the DHCP heredoc in `_generate_nmstate_dhcp_entries`. Earlier test runs matched it (reformatting the heredoc, `remove-prefix: "- "`, a quoted-MAC param, a doc `remove-text` for the MTU comment). This batch ignored it as having no repo source instead.

**Decide:** match it to the heredoc (as in the test runs) or keep it ignored.
