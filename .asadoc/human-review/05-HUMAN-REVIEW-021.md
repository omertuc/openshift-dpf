# HUMAN-REVIEW-021: `openshift-docs:nw-dpf-approving-worker-csrs/terminal-002`

- **Priority:** 5
- **Status:** ignored (manual-command)
- **TODO:** `# TODO` comment in `scripts/worker.sh` (no marker for this block)

The doc approves CSRs with a go-template piped to `xargs`. `approve_worker_csrs` uses the same go-template in a loop with per-CSR logging. Matching would drop the logging.

**Decide:** whether ignoring is right.
