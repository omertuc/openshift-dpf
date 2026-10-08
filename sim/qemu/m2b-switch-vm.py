#!/usr/bin/env python3
"""Switch an M2b host VM to the patched QEMU so its igb pair looks like a
BlueField-3 to DPF's host components: emulator -> qemu-kvm-bf3sim, the two igb
NICs become functions 0 and 1 of one multifunction slot, and both get
x-vpd-serial=<serial> (a BlueField's two PFs report the same serial).
Function 0 also gets x-pcie-ari-nextfn-1: igb has an ARI capability, and
with ARI the guest only finds the functions the ARI next-function chain
names, which is just function 0 by default.
Both also get x-vf-loopback=off: a BlueField sends all VF traffic to its
eswitch (on the DPU), never VF to VF inside the NIC, so neither may igb.
For the same reason both NICs get <port isolated='yes'/>: on the wire bridge
(sim/fabric/wire.sh) they may only talk to the DPU, not to each other.

Run on the hypervisor. Prints the new domain XML; define it with virsh.

usage: m2b-switch-vm.py <domain.xml> <emulator> <serial> <mac-p0> <mac-p1>
"""
import sys
import xml.etree.ElementTree as ET

QEMU_NS = "http://libvirt.org/schemas/domain/qemu/1.0"
ET.register_namespace("qemu", QEMU_NS)


def main(path, emulator, serial, mac0, mac1):
    tree = ET.parse(path)
    dom = tree.getroot()
    for attr in ("id",):
        dom.attrib.pop(attr, None)
    dom.find("devices/emulator").text = emulator

    nics = {}
    for iface in dom.findall("devices/interface"):
        mac = iface.find("mac").get("address").lower()
        if mac in (mac0.lower(), mac1.lower()):
            nics[mac] = iface
    if len(nics) != 2:
        sys.exit(f"expected both igb NICs {mac0} {mac1}, found {sorted(nics)}")

    p0, p1 = nics[mac0.lower()], nics[mac1.lower()]
    addr0 = p0.find("address")
    bus, slot = addr0.get("bus"), addr0.get("slot")
    for fn, iface in ((0, p0), (1, p1)):
        a = iface.find("address")
        a.attrib.update({"type": "pci", "domain": "0x0000", "bus": bus, "slot": slot, "function": f"0x{fn:x}"})
        if fn == 0:
            a.set("multifunction", "on")
        else:
            a.attrib.pop("multifunction", None)
        if iface.find("port") is None:
            ET.SubElement(iface, "port", {"isolated": "yes"})
        # live XML carries runtime-only elements
        for tag in ("target", "alias"):
            for e in iface.findall(tag):
                if tag == "target" or e.get("name", "").startswith("net"):
                    iface.remove(e)

    # Give the NICs user aliases so qemu:override can address them.
    for name, iface in (("ua-bf3p0", p0), ("ua-bf3p1", p1)):
        alias = ET.SubElement(iface, "alias")
        alias.set("name", name)

    override = dom.find(f"{{{QEMU_NS}}}override")
    if override is None:
        override = ET.SubElement(dom, f"{{{QEMU_NS}}}override")
    for name in ("ua-bf3p0", "ua-bf3p1"):
        dev = ET.SubElement(override, f"{{{QEMU_NS}}}device", {"alias": name})
        fe = ET.SubElement(dev, f"{{{QEMU_NS}}}frontend")
        ET.SubElement(fe, f"{{{QEMU_NS}}}property", {"name": "x-vpd-serial", "type": "string", "value": serial})
        ET.SubElement(fe, f"{{{QEMU_NS}}}property", {"name": "x-vf-loopback", "type": "bool", "value": "false"})
        if name == "ua-bf3p0":
            ET.SubElement(fe, f"{{{QEMU_NS}}}property", {"name": "x-pcie-ari-nextfn-1", "type": "bool", "value": "true"})

    tree.write(sys.stdout, encoding="unicode")


if __name__ == "__main__":
    if len(sys.argv) != 6:
        sys.exit(__doc__)
    main(*sys.argv[1:])
