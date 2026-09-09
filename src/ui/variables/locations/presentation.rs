//! Storage-address presentation using the current stop's mapping snapshot.

use crate::debugger::{
    MemoryKind,
    context::{MemoryRegion, memory_region_for_address},
    location::ValueLocation,
};

#[derive(Clone)]
pub(super) struct Presentation {
    pub text: String,
    pub tooltip: String,
    pub kind: MemoryKind,
}

pub(super) fn format(
    location: Option<&ValueLocation>,
    current: bool,
    bits: u32,
    regions: Option<&[MemoryRegion]>,
) -> Presentation {
    let location = location.filter(|_| current);
    let (text, detail) = match location {
        Some(ValueLocation::Memory {
            address,
            referenced,
        }) => {
            let width = (bits / 4).clamp(1, 16) as usize;
            let text = format!(
                "0x{address:0width$x}{}",
                if *referenced { " (referent)" } else { "" }
            );

            let detail = if *referenced {
                "Address of the referenced object, not storage for the reference itself"
            } else {
                "Storage address reported by GDB. For a pointer this is the pointer's own storage, not its target. Bitfields may share a storage unit"
            };

            let region = regions.and_then(|regions| memory_region_for_address(regions, *address));
            let tooltip = match region {
                Some(region) => format!(
                    "Mapping: 0x{:0width$x}-0x{:0width$x}  {}  {}\n{detail}",
                    region.start,
                    region.end,
                    region.permissions,
                    region.path.as_deref().unwrap_or("anonymous"),
                ),
                None if regions.is_some_and(|regions| !regions.is_empty()) => {
                    format!("No known mapping contains this storage address\n{detail}")
                }
                None => format!("Memory mappings are unavailable for this stop\n{detail}"),
            };

            return Presentation {
                text,
                tooltip,
                kind: region.map_or(MemoryKind::None, |region| region.kind),
            };
        }
        Some(ValueLocation::NonAddressable) => (
            "No address",
            "GDB reports no addressable storage. This does not identify a register. Computed and synthetic values may also have no address",
        ),
        Some(ValueLocation::OptimizedOut) => (
            "Optimized out",
            "The compiler optimized out this value or part of it",
        ),
        Some(ValueLocation::Unavailable) => (
            "Unavailable",
            "GDB cannot access the target state needed to locate this value",
        ),
        Some(ValueLocation::Unknown(detail)) => ("Not resolved", detail.as_str()),
        None if current => ("…", "Resolving storage at the current stop"),
        None => ("—", "A current paused value is required"),
    };

    Presentation {
        text: text.into(),
        tooltip: detail.into(),
        kind: MemoryKind::None,
    }
}
