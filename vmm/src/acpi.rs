// Copyright © 2019 Intel Corporation
//
// SPDX-License-Identifier: Apache-2.0
//
use acpi_tables::{
    aml::Aml,
    rsdp::RSDP,
    sdt::{GenericAddress, SDT},
};
use vm_memory::{GuestAddress, GuestMemoryMmap};

use vm_memory::{Address, ByteValued, Bytes};

use std::sync::{Arc, Mutex};

use crate::cpu::CpuManager;
use crate::device_manager::DeviceManager;
use crate::memory_manager::MemoryManager;
use arch::layout;

#[repr(packed)]
#[derive(Default)]
struct PCIRangeEntry {
    pub base_address: u64,
    pub segment: u16,
    pub start: u8,
    pub end: u8,
    _reserved: u32,
}

#[repr(packed)]
#[derive(Default)]
struct IortParavirtIommuNode {
    pub type_: u8,
    pub length: u16,
    pub revision: u8,
    _reserved1: u32,
    pub num_id_mappings: u32,
    pub ref_id_mappings: u32,
    pub device_id: u32,
    _reserved2: [u32; 3],
    pub model: u32,
    pub flags: u32,
    _reserved3: [u32; 4],
}

#[repr(packed)]
#[derive(Default)]
struct IortPciRootComplexNode {
    pub type_: u8,
    pub length: u16,
    pub revision: u8,
    _reserved1: u32,
    pub num_id_mappings: u32,
    pub ref_id_mappings: u32,
    pub mem_access_props: IortMemoryAccessProperties,
    pub ats_attr: u32,
    pub pci_seg_num: u32,
    pub mem_addr_size_limit: u8,
    _reserved2: [u8; 3],
}

#[repr(packed)]
#[derive(Default)]
struct IortMemoryAccessProperties {
    pub cca: u32,
    pub ah: u8,
    _reserved: u16,
    pub maf: u8,
}

#[repr(packed)]
#[derive(Default)]
struct IortIdMapping {
    pub input_base: u32,
    pub num_of_ids: u32,
    pub ouput_base: u32,
    pub output_ref: u32,
    pub flags: u32,
}

pub fn create_dsdt_table(
    device_manager: &DeviceManager,
    cpu_manager: &Arc<Mutex<CpuManager>>,
    memory_manager: &Arc<Mutex<MemoryManager>>,
) -> SDT {
    // DSDT
    let mut dsdt = SDT::new(*b"DSDT", 36, 6, *b"CLOUDH", *b"CHDSDT  ", 1);

    dsdt.append_slice(device_manager.to_aml_bytes().as_slice());
    dsdt.append_slice(cpu_manager.lock().unwrap().to_aml_bytes().as_slice());
    dsdt.append_slice(memory_manager.lock().unwrap().to_aml_bytes().as_slice());

    dsdt
}

pub fn create_acpi_tables(
    guest_mem: &GuestMemoryMmap,
    device_manager: &DeviceManager,
    cpu_manager: &Arc<Mutex<CpuManager>>,
    memory_manager: &Arc<Mutex<MemoryManager>>,
) -> GuestAddress {
    // RSDP is at the EBDA
    let rsdp_offset = layout::RSDP_POINTER;
    let mut tables: Vec<u64> = Vec::new();

    // DSDT
    let dsdt = create_dsdt_table(device_manager, cpu_manager, memory_manager);
    let dsdt_offset = rsdp_offset.checked_add(RSDP::len() as u64).unwrap();
    guest_mem
        .write_slice(dsdt.as_slice(), dsdt_offset)
        .expect("Error writing DSDT table");

    // FACP aka FADT
    // Revision 6 of the ACPI FADT table is 276 bytes long
    let mut facp = SDT::new(*b"FACP", 276, 6, *b"CLOUDH", *b"CHFACP  ", 1);

    // HW_REDUCED_ACPI and RESET_REG_SUP
    let fadt_flags: u32 = 1 << 20 | 1 << 10;
    facp.write(112, fadt_flags);

    // RESET_REG
    facp.write(116, GenericAddress::io_port_address(0x3c0));
    // RESET_VALUE
    facp.write(128, 1u8);

    facp.write(131, 3u8); // FADT minor version
    facp.write(140, dsdt_offset.0); // X_DSDT

    // SLEEP_CONTROL_REG
    facp.write(244, GenericAddress::io_port_address(0x3c0));
    // SLEEP_STATUS_REG
    facp.write(256, GenericAddress::io_port_address(0x3c0));

    facp.write(268, b"CLOUDHYP"); // Hypervisor Vendor Identity

    facp.update_checksum();
    let facp_offset = dsdt_offset.checked_add(dsdt.len() as u64).unwrap();
    guest_mem
        .write_slice(facp.as_slice(), facp_offset)
        .expect("Error writing FACP table");
    tables.push(facp_offset.0);

    // MADT
    let madt = cpu_manager.lock().unwrap().create_madt();
    let madt_offset = facp_offset.checked_add(facp.len() as u64).unwrap();
    guest_mem
        .write_slice(madt.as_slice(), madt_offset)
        .expect("Error writing MADT table");
    tables.push(madt_offset.0);

    // MCFG
    let mut mcfg = SDT::new(*b"MCFG", 36, 1, *b"CLOUDH", *b"CHMCFG  ", 1);

    // MCFG reserved 8 bytes
    mcfg.append(0u64);

    // 32-bit PCI enhanced configuration mechanism
    mcfg.append(PCIRangeEntry {
        base_address: layout::PCI_MMCONFIG_START.0,
        segment: 0,
        start: 0,
        end: 0xff,
        ..Default::default()
    });

    let mcfg_offset = madt_offset.checked_add(madt.len() as u64).unwrap();
    guest_mem
        .write_slice(mcfg.as_slice(), mcfg_offset)
        .expect("Error writing MCFG table");
    tables.push(mcfg_offset.0);

    // XSDT
    let mut xsdt = SDT::new(*b"XSDT", 36, 1, *b"CLOUDH", *b"CHXSDT  ", 1);
    for table in tables {
        xsdt.append(table);
    }
    xsdt.update_checksum();

    let xsdt_offset = mcfg_offset.checked_add(mcfg.len() as u64).unwrap();
    guest_mem
        .write_slice(xsdt.as_slice(), xsdt_offset)
        .expect("Error writing XSDT table");

    // RSDP
    let rsdp = RSDP::new(*b"CLOUDH", xsdt_offset.0);
    guest_mem
        .write_slice(rsdp.as_slice(), rsdp_offset)
        .expect("Error writing RSDP");

    rsdp_offset
}
