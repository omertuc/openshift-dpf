# Simulated BlueField DPUs

The pieces [dpusimaas](https://github.com/rh-ecosystem-edge/dpusimaas) runs to give DPF simulated
DPUs. openshift-dpf's `SIMULATED_DPUS_ONLY` sets up the cluster side.

| Directory | What | Built by |
|---|---|---|
| `dpusim/` | Rust CLI. On the hypervisors: DPU VMs booted from DPF's bf.cfg (`dpu-vm`), host VMs with a patched-QEMU BlueField-3 (`host-vm`), the VXLAN links between them (`wire`). On the DPU VM: DPF's OVS bridges and representors (`dpu-ovs`), the BlueField hardware the dpu-agent sees (`agent-hw`). | dpusimaas `make build-dpusim` |
| `sf-device-plugin/` | Advertises simulated SFs (`nvidia.com/bf_sf`) on the DPU, for HBN. A static pod dpusim puts in the DPU's ignition. | `podman build` (see its Containerfile), `quay.io/otuchfel/sim-dpu:sf-device-plugin` |
| `ovs-cni/` | `sim-sf.patch` on DPF's ovs-cni (doca-platform v26.4.1): plugs a simulated SF into its pod as a veth. | `ovs-cni/build.sh`, `quay.io/otuchfel/sim-dpu:ovs-cni-simsf-veth2-arm64` |
