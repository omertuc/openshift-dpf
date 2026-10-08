#!/usr/bin/env python3
"""Live view of a simulated-DPU cluster: DPU phases per simulation level, the
hosted cluster's Nodes and CSRs, and local emulated DPU VMs (console tail and
first-boot state). Polls in the background and serves a page that refreshes
itself.

usage: KUBECONFIG=<mgmt> dashboard.py [--port 8099] [--vm name=port:console ...]
"""
import argparse
import base64
import http.server
import json
import os
import subprocess
import tempfile
import threading
import time

NS = "dpf-operator-system"
HERE = os.path.dirname(os.path.abspath(__file__))

state = {"updated": None, "dpus": [], "nodes": [], "csrs": [], "vms": [], "events": [], "errors": []}
lock = threading.Lock()


def run(cmd, timeout=30):
    r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    if r.returncode != 0:
        raise RuntimeError(f"{' '.join(cmd[:4])}: {r.stderr.strip()[:200]}")
    return r.stdout


def oc_json(*args, kubeconfig=None):
    cmd = ["oc"] + (["--kubeconfig", kubeconfig] if kubeconfig else []) + list(args) + ["-o", "json"]
    return json.loads(run(cmd))


def hosted_kubeconfig():
    secret = run(["oc", "get", "dpucluster", "-n", NS, "-o", "jsonpath={.items[0].spec.kubeconfig}"]).strip()
    data = oc_json("get", "secret", "-n", NS, secret)["data"]["super-admin.conf"]
    fd, path = tempfile.mkstemp(prefix="hosted-", suffix=".kubeconfig")
    with os.fdopen(fd, "wb") as f:
        f.write(base64.b64decode(data))
    return path


def collect_cluster(hosted):
    hosts = {n["metadata"]["name"]: n["metadata"].get("labels", {})
             for n in oc_json("get", "nodes")["items"]}
    dpus = []
    for d in oc_json("get", "dpu", "-n", NS)["items"]:
        host = d["spec"].get("dpuNodeName", "")
        dpus.append({
            "name": d["metadata"]["name"],
            "host": host,
            "level": hosts.get(host, {}).get("dpf.openshift.io/sim-level", "?"),
            "phase": d.get("status", {}).get("phase", ""),
            "created": d["metadata"]["creationTimestamp"],
        })
    nodes = []
    for n in oc_json("get", "nodes", kubeconfig=hosted)["items"]:
        conds = {c["type"]: c["status"] for c in n.get("status", {}).get("conditions", [])}
        labels = n["metadata"].get("labels", {})
        nodes.append({
            "name": n["metadata"]["name"],
            "ready": conds.get("Ready", "Unknown"),
            "kind": "simulated" if labels.get("node-role.dpf.nvidia.com/fake") == "true" else "real",
            "kubelet": n.get("status", {}).get("nodeInfo", {}).get("kubeletVersion", ""),
            "arch": n.get("status", {}).get("nodeInfo", {}).get("architecture", ""),
        })
    csrs = []
    for c in oc_json("get", "csr", kubeconfig=hosted)["items"]:
        conds = [x["type"] for x in c.get("status", {}).get("conditions", [])]
        csrs.append({
            "name": c["metadata"]["name"],
            "signer": c["spec"]["signerName"].split("/")[-1],
            "requestor": c["spec"].get("username", ""),
            "status": ",".join(conds) or "Pending",
            "created": c["metadata"]["creationTimestamp"],
        })
    csrs.sort(key=lambda c: c["created"], reverse=True)
    return dpus, nodes, csrs


VM_PROBE = r"""
echo "hostname=$(hostname)"
for u in machine-config-daemon-pull machine-config-daemon-firstboot crio kubelet; do
  echo "unit.$u=$(systemctl is-active $u 2>/dev/null)"
done
echo "booted=$(rpm-ostree status --json 2>/dev/null | python3 -c 'import json,sys; d=[x for x in json.load(sys.stdin)["deployments"] if x.get("booted")]; print((d[0].get("container-image-reference") or d[0].get("origin") or "")[-90:] if d else "")' 2>/dev/null)"
"""


def collect_vm(name, target, port, console):
    vm = {"name": name, "ssh": f"ssh -p {port} {target}" if port != 22 else f"ssh {target}",
          "reachable": False, "info": {}, "console": []}
    try:
        if not console:
            raise OSError("no serial console; SSH only")
        with open(console, "rb") as f:
            f.seek(0, 2)
            f.seek(max(0, f.tell() - 12000))
            lines = f.read().decode(errors="replace").replace("\r", "").splitlines()
        vm["console"] = [l for l in lines if l.strip()][-14:]
    except OSError as e:
        vm["console"] = [f"(no console: {e})"]
    try:
        out = run(["ssh", "-q", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5",
                   "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
                   "-p", str(port), target, VM_PROBE], timeout=20)
        vm["reachable"] = True
        vm["info"] = dict(l.split("=", 1) for l in out.splitlines() if "=" in l)
    except Exception:
        pass
    return vm


def poller(vms, interval):
    hosted = None
    last_phase, last_ready, last_csr = {}, {}, {}
    initialized = False
    while True:
        errors = []
        try:
            hosted = hosted or hosted_kubeconfig()
            dpus, nodes, csrs = collect_cluster(hosted)
        except Exception as e:
            errors.append(str(e))
            dpus, nodes, csrs = state["dpus"], state["nodes"], state["csrs"]
        vm_states = [collect_vm(*v) for v in vms]
        now = time.strftime("%H:%M:%S")
        events = []
        for d in dpus:
            if last_phase.get(d["name"]) != d["phase"]:
                if d["name"] in last_phase:
                    events.append({"t": now, "what": f"DPU {d['name']} ({d['level']}): {last_phase[d['name']] or '-'} → {d['phase']}"})
                last_phase[d["name"]] = d["phase"]
        for n in nodes:
            if last_ready.get(n["name"]) != n["ready"]:
                events.append({"t": now, "what": f"hosted Node {n['name']} ({n['kind']}): Ready={n['ready']}"})
                last_ready[n["name"]] = n["ready"]
        for c in csrs:
            # The first poll only records the existing CSRs.
            if last_csr.get(c["name"]) != c["status"] and initialized:
                events.append({"t": now, "what": f"CSR {c['name']} ({c['signer']}) by {c['requestor']}: {c['status']}"})
            last_csr[c["name"]] = c["status"]
        initialized = True
        with lock:
            state.update(updated=now, dpus=dpus, nodes=nodes, csrs=csrs, vms=vm_states, errors=errors)
            state["events"] = (events + state["events"])[:200]
        time.sleep(interval)


class Handler(http.server.SimpleHTTPRequestHandler):
    def __init__(self, *a, **kw):
        super().__init__(*a, directory=HERE, **kw)

    def do_GET(self):
        if self.path.startswith("/status.json"):
            with lock:
                body = json.dumps(state).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Cache-Control", "no-store")
            self.end_headers()
            self.wfile.write(body)
            return
        if self.path == "/":
            self.path = "/index.html"
        super().do_GET()

    def log_message(self, *a):
        pass


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--port", type=int, default=8099)
    p.add_argument("--interval", type=int, default=10)
    p.add_argument("--vm", action="append", default=[], help="name=sshport:consolelog (local VM, core@127.0.0.1)")
    p.add_argument("--host", action="append", default=[], help="name=user@host (remote machine, SSH only)")
    a = p.parse_args()
    vms = []
    for v in a.vm:
        name, rest = v.split("=", 1)
        port, console = rest.split(":", 1)
        vms.append((name, "core@127.0.0.1", int(port), console))
    for h in a.host:
        name, target = h.split("=", 1)
        vms.append((name, target, 22, None))
    threading.Thread(target=poller, args=(vms, a.interval), daemon=True).start()
    print(f"http://127.0.0.1:{a.port}/")
    http.server.ThreadingHTTPServer(("127.0.0.1", a.port), Handler).serve_forever()


if __name__ == "__main__":
    main()
