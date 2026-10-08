# Simulated DPUs

This directory runs DPF on OpenShift without BlueField hardware. It still exercises the
real DPF operator and the real dpf-hcp-provisioner-operator. Each "level" swaps one
more fake part for a real one.

Diagrams, a real-vs-sim comparison and costs for every level, including 100-DPU
estimates, are in [`dashboard/levels.html`](dashboard/levels.html). Serve it with the
dashboard; see [Dashboard](#dashboard).

| Level | Host | DPU | What it proves |
|---|---|---|---|
| M0 | fake Node (kwok) | fake Node created by mock-dms | DPF's objects and phases work end to end |
| M1 | fake Node | fake Node that joins from DPF's real ignition, with real CSR approval | the provisioner's ignition and CSR flow |
| M2a | fake Node + the **real hostagent** pod, fed fake BlueField sysfs | M1-style | the real host-side agent's state machine |
| M2b | **real host VM** whose NICs look like a BlueField (patched QEMU igb) | M1-style | real SR-IOV VFs and the real device plugins |
| M3 | M2a host | **real aarch64 VM** booting DPF's ignition | a real DPU node: kubelet, MCO, OVS, DPU services |
| M4 | M2b host VM | M3 DPU VM, **wired** to the host | real OVN-K host↔DPU pairing; pods on VFs get traffic |
| M5 | M4 | M4 + simulated SFs | HBN, DTS and service chains run on the DPU |
| M6 | 2+ M4 pairs | each DPU's uplink on a simulated leaf switch (FRR) | HBN BGP to a leaf; traffic across hosts |

The levels run side by side in one cluster. Every host has the label
`dpf.openshift.io/sim-level=<level>`, and the mock-dms and sim-dpu selectors keep the
levels from interfering with each other.

## Machines

| What | Where |
|---|---|
| Management cluster | `export KUBECONFIG=~/.kube/dpf-dev/profile-1/mgmt.kubeconfig` (cluster `omer-upgrade`) |
| Hosted (DPU) cluster kubeconfig | `s=$(oc get dpucluster -n dpf-operator-system -o jsonpath='{.items[0].spec.kubeconfig}'); oc get secret -n dpf-operator-system $s -o jsonpath='{.data.super-admin\.conf}' \| base64 -d > /tmp/hosted.kubeconfig` |
| **hv2** (x86 hypervisor, 10.6.135.44) | host VMs `vm-omertuc-wew-worker1/2`. The openshift-dpf checkout that created them is at `/root/user-envs/omertuc/openshift-dpf-m2b`. The patched QEMU is `/usr/libexec/qemu-kvm-bf3sim`, with its build files in `/root/user-envs/omertuc/qemu-bf3sim/` |
| **aarchv** (Ampere aarch64 hypervisor, 10.6.135.47) | DPU VMs `dpusim-*` and the leaf. **Other people use this machine, so keep RAM and disk use low.** The leaf script is at `/root/omer-dpu-sim/leaf.sh`. `rhel10.2-omer` (shut off) is the old "aarc" machine from before VMs; leave it alone without asking |

Wires between the hypervisors. Each host↔DPU pair gets its own VXLAN:

| Pair | hv2 bridge (host's two igb NICs) | VXLAN (VNI) | aarchv bridge (DPU's wire NIC) | aarchv fabric bridge (DPU's p0 → leaf) |
|---|---|---|---|---|
| worker1 `52-54-00-aa-fb-35` ↔ VM `dpusim-m4-dpu` | `dpusim0` | `vxdpusim` (4247) | `br-dpusim` | `br-dpufab0` (leaf `swp0`) |
| worker2 `52-54-00-d6-62-89` ↔ VM `dpusim-52-54-00-d6-62-89-mt26sim62371` | `dpusim1` | `vxdpusim1` (4248) | `br-dpusim1` | `br-dpufab1` (leaf `swp1`) |

Create a wire like this; the hv2 side mirrors the aarchv side:

```bash
# aarchv
ip link add br-dpusim1 type bridge && ip link set br-dpusim1 up
ip link add vxdpusim1 mtu 1450 type vxlan id 4248 local 10.6.135.47 remote 10.6.135.44 dstport 4789
ip link set vxdpusim1 master br-dpusim1 up
# hv2: the same, with bridge dpusim1 and local/remote swapped
```

**How the wire works.** The DPU VM has three NICs:

- **mgmt** on `mgmt-br`, the lab LAN with DHCP. It plays the BlueField's `pf0vf0` / OOB port.
- **wire**, the BlueField's PCIe side, carrying all the host's PF and VF traffic as one VLAN trunk:
  - untagged = host PF (`rep0-0`);
  - VLAN 100+N = host p0 VF N (`rep0-N`);
  - VLAN 200+N = host p1 VF N (`rep1-N`).

  The host tags its VFs (`bf3sim-host.sh`), and the DPU splits the trunk with a
  VLAN-filtering bridge called `dpuwire` (`m3/dpf-ovs-sim.sh`).
- **fabric**, named `p0`: the uplink to the leaf.

## Runbooks

### M0 / M1 / M2a: fake hosts (`scripts/sim.sh`)

```bash
export KUBECONFIG=~/.kube/dpf-dev/profile-1/mgmt.kubeconfig
export SIM_MOCK_DMS_IMAGE=quay.io/otuchfel/mock-dms:m2a SIM_DPU_IMAGE=quay.io/otuchfel/sim-dpu:m2a
make deploy-sim                                   # kwok (both clusters), mock-dms, sim-dpu, hosts
SIM_LEVEL=m1  SIM_HOST_PREFIX=sim-m1-host  SIM_NUM_HOSTS=3 scripts/sim.sh create-sim-hosts
SIM_LEVEL=m2a SIM_HOST_PREFIX=sim-m2a-host SIM_NUM_HOSTS=2 \
  SIM_HOSTAGENT_IMAGE=nvcr.io/nvidia/doca/hostdriver:v26.4.1 scripts/sim.sh create-sim-hosts
make delete-sim-hosts                             # removes all fake hosts
```

- **Serial numbers.** Fake hosts get a stable BlueField serial from the node name:
  `MT26SIM` plus 5 digits from cksum; see `sim_serial` in `scripts/sim.sh`.
- **MCO annotations.** `create-sim-hosts` puts the MCO "already on the rendered config"
  annotations on fake hosts. Without them the maintenance operator cannot pause their
  MachineConfigPool.
- **M2a fake hardware.** The fake BlueField sysfs, VPD and tool shims come from
  `manifests/sim/sim-hardware.yaml`. Each host's hostagent pod is
  `manifests/sim/hostagent-m2a.yaml`. Inside that pod, `sim-dpu --agent-host` plays
  the BlueField's dpu-agent.

### M2b: turn a real host VM into a "BlueField host"

1. Add the worker VM with two extra igb NICs on its wire bridge. On hv2, in the
   checkout:
   `make add-vm-workers VM_WORKER_COUNT=<n> VM_WORKER_EXTRA_NIC_BRIDGE=dpusim1 AUTO_APPROVE_WORKER_CSR=false`.
   Approve only that node's CSRs by hand.
2. Once MCO is Done, switch the VM to the patched QEMU. On hv2:
   ```bash
   cd /root/user-envs/omertuc/qemu-bf3sim
   V=vm-omertuc-wew-worker2
   virsh dumpxml --inactive $V > $V.backup.xml
   python3 m2b-switch-vm.py $V.backup.xml /usr/libexec/qemu-kvm-bf3sim <SERIAL> <p0-mac> <p1-mac> > $V.bf3sim.xml
   virsh define $V.bf3sim.xml && virsh destroy $V && virsh start $V
   ```
3. Copy `qemu/bf3sim-host.sh` to `/usr/local/bin/` and `qemu/bf3sim-host.service` to
   `/etc/systemd/system/` on the host, then enable the service. It makes p0/p1 report
   BlueField device id `0xa2dc`, creates 7 VFs per PF and tags them with VLANs. It also
   gives p0 the MAC that OVN-K expects for DHCP from the DPU.
4. Label the node `dpf.openshift.io/sim-level=m4 feature.node.kubernetes.io/dpu-enabled=`.
   Then create its hostagent pod; the serial comes from the node name, as with
   `sim_serial`:
   ```bash
   (source scripts/sim.sh
    SIM_HOSTAGENT_IMAGE=nvcr.io/nvidia/doca/hostdriver:v26.4.1 SIM_DPU_IMAGE=quay.io/otuchfel/sim-dpu:m2a-2
    create_sim_hostagent <node>)
   oc annotate node <node> provisioning.dpu.nvidia.com/override-dms-pod-name=sim-hostagent-<node> --overwrite
   ```
   The provisioning flow still goes through the fake-hardware hostagent pod (as in
   M2a). The real igb PFs/VFs are what the SR-IOV device plugin and OVN-K use.

**Building the patched QEMU** (once per hypervisor; RHEL 9.6 `qemu-kvm-9.1.0-15.el9_6.7`):

1. In a `registry.access.redhat.com/ubi9/ubi:9.6` container with the RHEL repos, run
   `rpm -i` on the src.rpm, then
   `dnf builddep --enablerepo=codeready-builder-for-rhel-9-x86_64-rpms qemu-kvm.spec`.
2. Copy `qemu/igb-vpd.patch` and `qemu/igb-no-vf-loopback.patch` to `SOURCES/` and add
   `Patch9999: igb-vpd.patch` and `Patch10000: igb-no-vf-loopback.patch` to the spec.
3. Run `rpmbuild -bc --define "dist .el9_6" SPECS/qemu-kvm.spec`.
4. Install `BUILD/qemu-9.1.0/qemu_kvm_build/qemu-system-x86_64` as
   `/usr/libexec/qemu-kvm-bf3sim`, then run `chcon -t qemu_exec_t` on it.

The patches add the `x-vpd-serial`, `x-pcie-ari-nextfn-1` and `x-vf-loopback` igb properties.
`x-vf-loopback=false` stops igb from switching VF-to-VF traffic inside the NIC, so it
always goes to the DPU, as on a BlueField (otherwise two pods on the same host PF can't talk).

### M3 / M4: DPU VM on aarchv

The host must already have a DPU CR whose phase has a bf.cfg, i.e. it is waiting for
the OS install. Then:

```bash
export KUBECONFIG=~/.kube/dpf-dev/profile-1/mgmt.kubeconfig
sim/m3/create-dpu-vm.sh <dpu-name>                                      # M3: no wire
sim/m3/create-dpu-vm.sh --wire-bridge br-dpusim1 --fabric-bridge br-dpufab1 <dpu-name>   # M4 + M6
sim/m3/create-dpu-vm.sh --delete <dpu-name>
```

`create-dpu-vm.sh` takes the DPU's bf.cfg from bfb-registry, then:
- turns it into a boot ignition (`m3/build-ignition.sh` and `m3/dpu-ignition.py`), which
  masks BlueField-only units and installs `dpf-ovs-sim.sh` and fwctl;
- creates a thin overlay on a shared RHCOS 4.22 aarch64 image;
- boots the VM with BlueField-3 SMBIOS (HBN checks for it).

The VM joins the hosted cluster as Node `<dpu-name>` in about 4 minutes, with no manual
steps. The NIC MACs are stable per DPU name.

The DPU's OVN-K runs with `--simulate-dpu`, from 4.23+, via the image overrides in
[Manual changes](#manual-changes-on-the-live-cluster).

### M5: simulated SFs (HBN, DTS)

- **Fake SFs.** Real DPU nodes get fake `nvidia.com/bf_sf` devices (`sim-sf-N`) from
  `sim-dpu --sf-device-plugin`:
  `sed -e 's|<SIM_DPU_IMAGE>|quay.io/otuchfel/sim-dpu:m5|' -e 's|<SF_COUNT>|16|' manifests/sim/sf-device-plugin.yaml | oc --kubeconfig /tmp/hosted.kubeconfig apply -f -`
- **ovs-cni.** DPF's ovs-cni is patched (`ovs-cni/sim-sf.patch`, on doca-platform
  `v26.4.1`). It turns a `sim-sf-*` device into a veth pair: the pod end is the SF
  (`p0_if`, `p1_if`, `pf2dpu2_if`), and the host end is a plain OVS port. Build it with
  `ovs-cni/build.sh <doca-platform@v26.4.1 with the patch applied> <image>`, then point
  DPF at it (see [Manual changes](#manual-changes-on-the-live-cluster)).

### M6: leaf switch and a second pair

- **Start the leaf.** On aarchv, run `bash /root/omer-dpu-sim/leaf.sh up 2`; the source
  is `fabric/leaf.sh`. It starts an FRR container `dpusim-leaf` (AS 65000) with ports
  `swpN` on `br-dpufabN`.
- **HBN peers with the leaf.** Each DPU's HBN uses BGP unnumbered on `p0_if`, with its
  own AS (65101 + last octet of its loopback).
- **Check it:**
  ```bash
  podman exec dpusim-leaf vtysh -c 'show bgp summary'                     # on aarchv
  oc --kubeconfig /tmp/hosted.kubeconfig -n dpf-operator-system exec <hbn-pod> -c doca-hbn -- vtysh -c 'show bgp summary'
  ```
- **Second pair:** a second host VM (M2b runbook), a second wire with VNI 4248, and
  `create-dpu-vm.sh --wire-bridge br-dpusim1 --fabric-bridge br-dpufab1`.

### Dashboard

```bash
KUBECONFIG=~/.kube/dpf-dev/profile-1/mgmt.kubeconfig python3 sim/dashboard/dashboard.py --port 8099
# http://127.0.0.1:8099 (live status), /levels.html (explainer)
```

## Manual changes on the live cluster

No script makes these changes yet. A fresh cluster needs all of them for M4+.

```bash
NS=dpf-operator-system
# More DPU nodes than openshift-dpf's hardcoded maxNodes: 10
oc patch dpucluster -n $NS doca --type=merge -p '{"spec":{"maxNodes":100}}'

# A large VTEP / pf2dpu pool (pool1 is a single /29). Fake nodes are excluded.
cat <<'EOF' | oc apply -f -
apiVersion: svc.dpu.nvidia.com/v1alpha1
kind: DPUServiceIPAM
metadata: {name: pool2, namespace: dpf-operator-system}
spec:
  ipv4Network: {network: 10.0.120.0/22, gatewayIndex: 3, prefixSize: 29}
  nodeSelector:
    nodeSelectorTerms:
    - matchExpressions: [{key: node-role.dpf.nvidia.com/fake, operator: DoesNotExist}]
EOF

# OVN-K on DPUs: simulate-dpu images, pool2 for the VTEP
oc patch dpuserviceconfiguration -n $NS ovn --type=merge -p '{"spec":{"serviceConfiguration":{"helmChart":{"values":{
  "gatewayOpts":"--gateway-interface=br-dpu --simulate-dpu",
  "dpuManifests":{"ipamPool":"pool2","vtepCIDR":"10.0.120.0/22",
    "image":{"repository":"quay.io/otuchfel/sim-dpu","tag":"ovnk-simdpu-arm64"},
    "imagedpf":{"repository":"quay.io/otuchfel/sim-dpu","tag":"ovn-dpf-utils-simdpu-arm64"}}}}}}}'

# HBN: 1Gi instead of 6Gi, 3 SFs, pf2dpu2 address from pool2
oc patch dpuserviceconfiguration -n $NS hbn --type=merge -p '{"spec":{"serviceConfiguration":{"helmChart":{"values":{"resources":{"memory":"1Gi","nvidia.com/bf_sf":3}}}}}}'
oc patch dpuserviceconfiguration -n $NS hbn --type=merge -p '{"spec":{"serviceConfiguration":{"serviceDaemonSet":{"annotations":{"k8s.v1.cni.cncf.io/networks":"[\n{\"name\": \"iprequest\", \"interface\": \"ip_lo\", \"cni-args\": {\"poolNames\": [\"loopback\"], \"poolType\": \"cidrpool\"}},\n{\"name\": \"iprequest\", \"interface\": \"ip_pf2dpu2\", \"cni-args\": {\"poolNames\": [\"pool2\"], \"poolType\": \"cidrpool\", \"allocateDefaultGateway\": true}}\n]"}}}}}'

# Simulated-SF ovs-cni
oc patch dpfoperatorconfig -n $NS dpfoperatorconfig --type=merge -p '{"spec":{"ovsCNI":{"cni":{"image":"quay.io/otuchfel/sim-dpu:ovs-cni-simsf-veth2-arm64"}}}}'

# Host VF pool: igb VFs are not RDMA-capable, so drop isRdma (live value below)
oc patch nodesriovdevicepluginconfig -n $NS bf3-vfs --type=json -p '[{"op":"replace","path":"/spec/devicePluginResources","value":[
  {"name":"bf3-p0-vfs-mgmt","type":"vf","ranges":[{"pfIndex":0,"start":1,"end":1}]},
  {"name":"bf3_vfs","type":"vf","ranges":[{"pfIndex":0,"start":2,"end":45},{"pfIndex":1,"start":0,"end":45}]}]}]'
```

Leftovers:
- `pool1` still has a nodeSelector requiring `dpf.openshift.io/sim-vtep=true`. That was
  an earlier workaround, now superseded by pool2.
- If you change OVN's pool on a live DPU, also do the stale-route cleanup in
  [Known bugs](#known-dpf--ovn-k-bugs-and-workarounds).

## Images (`quay.io/otuchfel/...`)

| Image | What | Built by |
|---|---|---|
| `mock-dms:m2a` (also `:m1`) | NVIDIA mock-dms + `mock-dms/*.patch` | `mock-dms/build.sh` |
| `sim-dpu:m2a`, `:m2a-2`, `:m5` (`:m1c` older) | our Go tool `dpu/`: DPU joiner, sfc / node-status stand-ins, dpu-agent (`--agent-host`), SF device plugin (`--sf-device-plugin`). `m5` is multi-arch | `podman build -f sim/dpu/Containerfile sim/dpu` |
| `sim-dpu:ovnk-simdpu-arm64` | DPF's 4.22 arm64 OVN-K image with go-controller binaries from openshift/ovn-kubernetes `release-4.23` @ `42d40a055` (no patches) | `ovnk/build.sh` |
| `sim-dpu:ovn-dpf-utils-simdpu-arm64` | ovn-kubernetes-dpf `v26.4.1-ocp-release-v4.22` + `ovn-dpf-utils/sim-dpu.patch` (cniprovisioner simulated-DPU mode) | `ovn-dpf-utils/build.sh` |
| `sim-dpu:ovs-cni-simsf-veth2-arm64` (current; `-veth-` and `ovs-cni-simsf-arm64` are older, broken) | DPF ovs-cni `v26.4.1` + `ovs-cni/sim-sf.patch` | `ovs-cni/build.sh` |
| `nvcr.io/nvidia/doca/hostdriver:v26.4.1` | NVIDIA's real hostagent (M2a+) | n/a |

The DPU VMs boot stock RHCOS 4.22 aarch64. MCO then rebases them to
`quay.io/edge-infrastructure/bluefield-ocp:4.22.7`, which ships OVS-DOCA. We still run
OVS's kernel datapath, because OVS-DOCA's datapath takes no veths.

## Known DPF / OVN-K bugs and workarounds

Worth reporting to NVIDIA.

- **Finalizers get stuck when a DPU node disappears.** ServiceChain and ServiceInterface
  objects for a deleted DPU node keep their finalizers forever. Workaround:
  `oc --kubeconfig /tmp/hosted.kubeconfig patch servicechain|serviceinterface -n dpf-operator-system <name> --type=merge -p '{"metadata":{"finalizers":null}}'`.
- **cniprovisioner is not idempotent across a pool change.** After the VTEP pool
  changes, it fails with "file exists" on a stale route. Workaround, on the DPU: delete
  routes via the old gateway and the old `br-ovn` address, then restart the OVN pod.
  ```bash
  ip route show | grep 'via 10.6.156.' | while read -r r; do sudo ip route del $r; done
  sudo ip addr del <old-ip>/29 dev br-ovn
  ```
- **nv-ipam keeps stale allocations after an abrupt DPU reboot.** Workaround, on the DPU:
  `sudo mv /var/lib/cni/nv-ipam/store /var/lib/cni/nv-ipam/store.stale-$(date +%s)`.
  Then delete that node's `ipam-node`, `ovn-kubernetes-node` and `doca-hbn` pods.
- **Paused old DPUService versions block the DPUDeployment.** After a rollout, DPF marks
  old versions' Argo Applications `argocd.argoproj.io/skip-reconcile=true`. Their health
  then stays frozen at whatever it was, e.g. "Progressing 9/10", so the DPUDeployment
  never turns Ready and a DPU sits in "Node Effect Removal". Workaround: refresh once.
  DPF puts the pause back.
  ```bash
  oc annotate application -n dpf-operator-system <app> argocd.argoproj.io/skip-reconcile- argocd.argoproj.io/refresh=hard --overwrite
  ```
- **sfc-controller and OVN-K fight over physical ports.** OVN-K deletes any OVS port with
  `ofport=-1`. The sfc-controller re-adds a missing physical port (`p0`/`p1`) as
  `type=dpdk`, which the kernel datapath cannot open, so it gets `ofport=-1` again, and
  so on. The ServiceChain then fails with "invalid or unknown port for in_port".
  Workaround, on the DPU: `sudo ovs-vsctl set interface p1 type=system`. The
  sfc-controller leaves existing ports alone, so this sticks until the port is deleted
  again.
- **openshift-dpf's defaults are too small for simulation.** `maxNodes: 10` is hardcoded
  in the DPUCluster, and `VTEP_CIDR` is a single /29.

## State (2026-10-08) and open problems

- **Running:**
  - M0 hosts `sim-host-0/1`, M1 hosts `sim-m1-host-0..2`, M2a hosts
    `sim-m2a-host-0/1` and `sim-m2a-reboot-0`;
  - M4 pair worker1 ↔ `dpusim-m4-dpu`: DPU Ready, DPUDeployment Success, HBN `p0_if` ↔
    leaf `swp0` BGP Established.
- **worker2** (`52-54-00-d6-62-89`, serial `MT26SIM62371`) is installed. Its DPU VM
  `dpusim-52-54-00-d6-62-89-mt26sim62371` exists on aarchv, but the pairing is not
  finished. The host stays NotReady until its DPU's OVN-K is up; that is expected in
  DPU-host mode.
- **Same-PF VF→VF pod traffic fails.** Two pods on VFs of the same host PF can't ping
  each other: QEMU's igb switches frames between VFs of one PF itself, the VEB loopback
  the Linux igb driver turns on, and it ignores the per-VF VLANs, so the frames never
  reach the DPU. Pods on different PFs, and pod→gateway, work. Under investigation;
  likely fix: patch QEMU igb to skip local VF switching, or disable it. Test pods are
  `vfpod-a/b` in namespace `dpf-sim-test`.
- **p1 VF 0** is an ordinary VF (VLAN 200, rep `rep1-0`). Only p0 VF 0 is the
  host↔DPU channel. A host and DPU set up before this change need
  `ip link set p1 vf 0 vlan 200` on the host and `rep1-0` on the DPU, or a reboot with
  the new scripts.
- **Untested:** pod traffic across hosts through HBN and the leaf.

## Pinned ideas

- **netdevsim kernel.** Build a kernel with netdevsim to get real switchdev
  representors and SFs instead of patching DPF. Building a kernel is not trivial, so
  this is parked.
- **M7.**
  - **Lifecycle/chaos:** reboot, reprovision and DPU loss mid-flight.
  - **One-command CI run** of a chosen level.
  - **Swarm mode:** many DPU kubelets in one VM, the assisted-swarm idea; see the costs
    tab.
