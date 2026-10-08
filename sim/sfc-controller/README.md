# sfc-controller with a configurable port type

DPF's sfc-controller adds the ports of ServiceInterfaces (p0/p1, PF, VF, SF
ports) to `br-sfc` as `type=dpdk`. A simulated DPU runs OVS's kernel datapath,
where a `dpdk` port never gets an ofport (and OVN-K then deletes it, in a
loop). `sim-port-type.patch` (on doca-platform `v26.4.1`) makes the type come
from `DPF_SIM_PORT_TYPE` (default `dpdk`, unchanged); the image sets it to
`system`.

Build and push (arm64, layered on the deployed `dpf-system:v26.4.1`):

```bash
dpusim image sfc-controller        # quay.io/otuchfel/sim-dpu:sfc-controller-simport-arm64
```

Use it on the DPUs (only the sfc-controller DaemonSet changes; DPF's other
components keep the stock dpf-system image):

```bash
oc patch dpfoperatorconfig -n dpf-operator-system dpfoperatorconfig --type=merge \
  -p '{"spec":{"sfcController":{"controller":{"image":"quay.io/otuchfel/sim-dpu:sfc-controller-simport-arm64"}}}}'
```

Ports that already exist as `type=dpdk` keep that type (the controller leaves
existing ports alone); reset them once with `ovs-vsctl set interface <port> type=system`.
