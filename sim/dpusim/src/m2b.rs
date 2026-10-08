//! Switches an M2b host VM to the patched QEMU so its igb pair looks like a
//! BlueField-3 to DPF's host components: emulator -> qemu-kvm-bf3sim, the two
//! igb NICs become functions 0 and 1 of one multifunction slot, and both get
//! x-vpd-serial=<serial> (a BlueField's two PFs report the same serial).
//! Function 0 also gets x-pcie-ari-nextfn-1: igb has an ARI capability, and
//! with ARI the guest only finds the functions the ARI next-function chain
//! names, which is just function 0 by default.
//! Both also get x-vf-loopback=off: a BlueField sends all VF traffic to its
//! eswitch (on the DPU), never VF to VF inside the NIC, so neither may igb.
//! For the same reason both NICs get `<port isolated='yes'/>`: on the wire
//! bridge (`wire`) they may only talk to the DPU, not to each other.
//!
//! Run on the hypervisor. Prints the new domain XML; define it with virsh.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Args;
use xmltree::{Element, EmitterConfig, Namespace, XMLNode};

use crate::mac::parse_mac;

const QEMU_NAMESPACE: &str = "http://libvirt.org/schemas/domain/qemu/1.0";
const QEMU_PREFIX: &str = "qemu";

#[derive(Args)]
pub struct M2bSwitchVmArgs {
    /// `virsh dumpxml --inactive` of the VM.
    domain_xml: PathBuf,
    /// The patched QEMU, e.g. /usr/libexec/qemu-kvm-bf3sim.
    emulator: String,
    /// The DPU serial both PFs report in their VPD.
    serial: String,
    #[arg(value_parser = parse_mac)]
    p0_mac: String,
    #[arg(value_parser = parse_mac)]
    p1_mac: String,
}

pub fn run(switch_args: &M2bSwitchVmArgs) -> Result<()> {
    let domain_xml = fs::read_to_string(&switch_args.domain_xml)
        .with_context(|| format!("reading {}", switch_args.domain_xml.display()))?;
    println!("{}", bf3sim_domain(&domain_xml, switch_args)?);
    Ok(())
}

/// One of the two igb NICs that become the BlueField's PFs.
struct Bf3Pf<'a> {
    mac: &'a str,
    function: u8,
    alias: &'static str,
}

impl Bf3Pf<'_> {
    fn qemu_properties(&self, serial: &str) -> Vec<(&'static str, &'static str, String)> {
        let common = [
            ("x-vpd-serial", "string", serial.to_owned()),
            ("x-vf-loopback", "bool", "false".to_owned()),
        ];
        let ari = (self.function == 0).then(|| ("x-pcie-ari-nextfn-1", "bool", "true".to_owned()));
        common.into_iter().chain(ari).collect()
    }
}

fn bf3sim_domain(domain_xml: &str, switch_args: &M2bSwitchVmArgs) -> Result<String> {
    let mut domain = Element::parse(domain_xml.as_bytes()).context("parsing the domain XML")?;
    domain.attributes.shift_remove("id");
    let pfs = [
        Bf3Pf {
            mac: &switch_args.p0_mac,
            function: 0,
            alias: "ua-bf3p0",
        },
        Bf3Pf {
            mac: &switch_args.p1_mac,
            function: 1,
            alias: "ua-bf3p1",
        },
    ];

    let devices = domain
        .get_mut_child("devices")
        .context("domain has no <devices>")?;
    let emulator = devices
        .get_mut_child("emulator")
        .context("domain has no <emulator>")?;
    emulator.children = vec![XMLNode::Text(switch_args.emulator.clone())];
    let (bus, slot) = pci_bus_and_slot(devices, switch_args.p0_mac.as_str())?;
    pfs.iter().try_for_each(|pf| {
        let interface = interface_with_mac(devices, pf.mac)?;
        make_bf3_pf(interface, pf, &bus, &slot)
    })?;

    let overrides = qemu_override(&mut domain)?;
    overrides.children.retain(|child| {
        child
            .as_element()
            .and_then(|device| device.attributes.get("alias"))
            .is_none_or(|alias| pfs.iter().all(|pf| pf.alias != alias))
    });
    overrides.children.extend(
        pfs.iter()
            .map(|pf| XMLNode::Element(qemu_device_override(pf, &switch_args.serial))),
    );

    let mut output = Vec::new();
    domain
        .write_with_config(
            &mut output,
            EmitterConfig::new()
                .perform_indent(true)
                .write_document_declaration(false),
        )
        .context("writing the domain XML")?;
    String::from_utf8(output).context("decoding the written domain XML")
}

fn interfaces(devices: &Element) -> impl Iterator<Item = &Element> {
    devices
        .children
        .iter()
        .filter_map(XMLNode::as_element)
        .filter(|child| child.name == "interface")
}

fn interface_mac(interface: &Element) -> Option<String> {
    interface
        .get_child("mac")
        .and_then(|mac| mac.attributes.get("address"))
        .map(|address| address.to_ascii_lowercase())
}

/// Where p0 sits now; p1 joins it there as function 1.
fn pci_bus_and_slot(devices: &Element, p0_mac: &str) -> Result<(String, String)> {
    let found_macs: Vec<String> = interfaces(devices).filter_map(interface_mac).collect();
    let p0 = interfaces(devices)
        .find(|interface| interface_mac(interface).as_deref() == Some(p0_mac))
        .with_context(|| format!("no NIC with MAC {p0_mac}; the domain has {found_macs:?}"))?;
    let address = p0
        .get_child("address")
        .with_context(|| format!("NIC {p0_mac} has no <address>"))?;
    let attribute = |name: &str| {
        address
            .attributes
            .get(name)
            .cloned()
            .with_context(|| format!("NIC {p0_mac} <address> has no {name}"))
    };
    Ok((attribute("bus")?, attribute("slot")?))
}

fn interface_with_mac<'a>(devices: &'a mut Element, mac: &str) -> Result<&'a mut Element> {
    let mut matching = devices
        .children
        .iter_mut()
        .filter_map(XMLNode::as_mut_element)
        .filter(|child| child.name == "interface" && interface_mac(child).as_deref() == Some(mac));
    let Some(interface) = matching.next() else {
        bail!("no NIC with MAC {mac}");
    };
    if matching.next().is_some() {
        bail!("more than one NIC with MAC {mac}");
    }
    Ok(interface)
}

fn element_with_attributes(name: &str, attributes: &[(&str, &str)]) -> Element {
    let mut element = Element::new(name);
    element.attributes = attributes
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
    element
}

fn make_bf3_pf(interface: &mut Element, pf: &Bf3Pf, bus: &str, slot: &str) -> Result<()> {
    let address = interface
        .get_mut_child("address")
        .with_context(|| format!("NIC {} has no <address>", pf.mac))?;
    let function = format!("0x{:x}", pf.function);
    address.attributes.extend(
        [
            ("type", "pci"),
            ("domain", "0x0000"),
            ("bus", bus),
            ("slot", slot),
            ("function", &function),
        ]
        .map(|(key, value)| (key.to_owned(), value.to_owned())),
    );
    if pf.function == 0 {
        address
            .attributes
            .insert("multifunction".to_owned(), "on".to_owned());
    } else {
        address.attributes.shift_remove("multifunction");
    }

    if interface.get_child("port").is_none() {
        interface
            .children
            .push(XMLNode::Element(element_with_attributes(
                "port",
                &[("isolated", "yes")],
            )));
    }
    // Live XML carries runtime-only <target> and <alias name='netN'>; the
    // user alias lets qemu:override address the NIC.
    interface.children.retain(|child| {
        child
            .as_element()
            .is_none_or(|element| element.name != "target" && element.name != "alias")
    });
    interface
        .children
        .push(XMLNode::Element(element_with_attributes(
            "alias",
            &[("name", pf.alias)],
        )));
    Ok(())
}

fn qemu_element(name: &str, attributes: &[(&str, &str)]) -> Element {
    let mut namespaces = Namespace::empty();
    namespaces.put(QEMU_PREFIX, QEMU_NAMESPACE);
    Element {
        prefix: Some(QEMU_PREFIX.to_owned()),
        namespace: Some(QEMU_NAMESPACE.to_owned()),
        namespaces: Some(namespaces),
        ..element_with_attributes(name, attributes)
    }
}

/// The domain's `<qemu:override>`, created if missing.
fn qemu_override(domain: &mut Element) -> Result<&mut Element> {
    let is_override = |element: &Element| {
        element.name == "override" && element.namespace.as_deref() == Some(QEMU_NAMESPACE)
    };
    if !domain
        .children
        .iter()
        .filter_map(XMLNode::as_element)
        .any(is_override)
    {
        domain
            .children
            .push(XMLNode::Element(qemu_element("override", &[])));
    }
    domain
        .children
        .iter_mut()
        .filter_map(XMLNode::as_mut_element)
        .find(|element| is_override(element))
        .context("domain has no <qemu:override>")
}

fn qemu_device_override(pf: &Bf3Pf, serial: &str) -> Element {
    let properties = pf
        .qemu_properties(serial)
        .into_iter()
        .map(|(name, kind, value)| {
            XMLNode::Element(qemu_element(
                "property",
                &[("name", name), ("type", kind), ("value", &value)],
            ))
        })
        .collect();
    let frontend = Element {
        children: properties,
        ..qemu_element("frontend", &[])
    };
    Element {
        children: vec![XMLNode::Element(frontend)],
        ..qemu_element("device", &[("alias", pf.alias)])
    }
}
